//! Hardware acceptance only; not included in the CPU detector test command.
//! CP_TEST_NER_DEVICE=cuda[:ids]|cann[:ids] CP_DETECTOR_ORT_LIBRARY=/path/to/gpu/libonnxruntime.so
//! cargo test --locked --test ner_accelerator -- --ignored --nocapture --test-threads=1
use censorprivacy::professional::{NerDevice, ProfessionalConfig, ProfessionalDetector};

#[test]
#[ignore = "requires CUDA or Ascend CANN hardware and compatible ONNX Runtime/provider dependencies"]
fn selected_accelerator_devices_and_batch_match_cpu_entities() {
    let device: NerDevice = std::env::var("CP_TEST_NER_DEVICE")
        .expect("set CP_TEST_NER_DEVICE=cuda[:ids] or cann[:ids]")
        .parse()
        .unwrap();
    assert!(matches!(device, NerDevice::Cuda(_) | NerDevice::Cann(_)));
    let texts = [
        "😀张三在清华大学工作。".to_owned(),
        format!(
            "{}张三在清华大学工作。{}李四在北京大学工作。",
            "普通测试文本。".repeat(72),
            "普通测试文本。".repeat(260)
        ),
    ];
    let cpu = ProfessionalDetector::start(ProfessionalConfig {
        ner_threads: 4,
        ..Default::default()
    })
    .unwrap();
    let expected: Vec<_> = texts
        .iter()
        .map(|text| cpu.ner_entities(text).unwrap())
        .collect();
    drop(cpu);
    for size in [1, 4] {
        let gpu = ProfessionalDetector::start(ProfessionalConfig {
            ner_device: device.clone(),
            ner_batch_size: size,
            ..Default::default()
        })
        .unwrap();
        let started = std::time::Instant::now();
        for (text, expected) in texts.iter().zip(&expected) {
            let actual = gpu.ner_entities(text).unwrap();
            assert_eq!(actual.len(), expected.len());
            for (a, b) in actual.iter().zip(expected) {
                assert_eq!((&a.label, a.start, a.end), (&b.label, b.start, b.end));
                assert!((a.score - b.score).abs() <= 1e-4);
            }
        }
        println!(
            "device={device}, batch={size}, elapsed_ms={}",
            started.elapsed().as_millis()
        );
        drop(gpu);
    }
}
