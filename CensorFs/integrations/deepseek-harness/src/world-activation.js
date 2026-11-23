// DSH owns startup cancellation. Once accepted, cancellation targets the
// specific activation, including when the caller aborts during publication.
export async function startWorldActivation(subagents, spec) {
  const run = await subagents.startActivation({ ...spec, delivery: 'caller' })
  const cancel = () => { void run.dispose().catch(() => {}) }
  spec.signal.addEventListener('abort', cancel, { once: true })
  if (spec.signal.aborted) cancel()
  return {
    childId: run.childId,
    result: run.result.finally(() => spec.signal.removeEventListener('abort', cancel)),
    dispose: () => run.dispose(),
  }
}
