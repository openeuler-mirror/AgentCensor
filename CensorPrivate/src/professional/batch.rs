//! One in-flight batch per worker. Results remain in document order, even when
//! devices finish out of order. No queue across requests and no batching delay.
use crate::error::{Error, Result};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(super) fn dispatch<W: Send, I: Sync, O: Send>(
    workers: &mut [W],
    items: &[I],
    batch_size: usize,
    run: impl Fn(&mut W, &[I]) -> Result<O> + Sync,
) -> Result<Vec<O>> {
    if workers.is_empty() || batch_size == 0 {
        return Err(Error::NerConfigInvalid);
    }
    let batches: Vec<_> = items.chunks(batch_size).collect();
    let active = workers.len().min(batches.len());
    if active <= 1 {
        return batches
            .iter()
            .map(|batch| run(&mut workers[0], batch))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut spawn_failed = false;
        for worker in workers.iter_mut().take(active) {
            let (next, failed, batches, run) = (&next, &failed, &batches, &run);
            let handle = std::thread::Builder::new()
                .name("ner-device".into())
                .spawn_scoped(scope, move || {
                    let mut results = Vec::new();
                    while !failed.load(Ordering::Acquire) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(batch) = batches.get(index) else {
                            break;
                        };
                        match run(worker, batch) {
                            Ok(result) => results.push((index, result)),
                            Err(error) => {
                                failed.store(true, Ordering::Release);
                                return Err(error);
                            }
                        }
                    }
                    Ok(results)
                });
            match handle {
                Ok(handle) => handles.push(handle),
                Err(_) => {
                    failed.store(true, Ordering::Release);
                    spawn_failed = true;
                    break;
                }
            }
        }
        let mut results = Vec::new();
        let mut error = spawn_failed.then_some(Error::DetectionFailed);
        // Always join every worker, including after an error. Never return a
        // partially scanned document or leave a device accessing request data.
        for handle in handles {
            match handle.join() {
                Ok(Ok(batch)) => results.extend(batch),
                Ok(Err(failure)) => {
                    error = Some(failure);
                }
                Err(_) => {
                    failed.store(true, Ordering::Release);
                    error = Some(Error::DetectionFailed);
                }
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        if results.len() != batches.len() {
            return Err(Error::DetectionFailed);
        }
        results.sort_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_workers_process_each_window_once_and_restore_document_order() {
        let barrier = std::sync::Barrier::new(3);
        let mut workers = [false; 3];
        let input: Vec<_> = (0..23).collect();
        let results = dispatch(&mut workers, &input, 4, |first, batch| {
            if !*first {
                *first = true;
                barrier.wait(); // Require three genuinely concurrent workers.
            }
            Ok(batch.to_vec())
        })
        .unwrap();
        assert!(workers.iter().all(|used| *used));
        assert_eq!(
            results.iter().map(Vec::len).collect::<Vec<_>>(),
            [4, 4, 4, 4, 4, 3]
        );
        assert_eq!(results.concat(), input);
    }
    #[test]
    fn device_error_rejects_all_results_and_small_requests_do_not_wait_for_a_full_batch() {
        let mut workers = [(); 3];
        assert_eq!(
            dispatch(&mut workers, &[1, 2], 4, |_, batch| Ok(batch.to_vec())).unwrap(),
            [vec![1, 2]]
        );
        let result = dispatch(&mut workers, &[0, 1, 2, 3, 4, 5], 1, |_, batch| {
            if batch[0] == 2 {
                Err(Error::DetectionFailed)
            } else {
                Ok(batch[0])
            }
        });
        assert_eq!(result, Err(Error::DetectionFailed));
    }
}
