/** Keep native live-stream identities dense across worker replacements. */
export class AssistantStreamBridge {
  constructor(publish) {
    this.publish = publish
    this.revision = 0
    this.generation = 0
    this.active = undefined
  }

  startWorker() {
    this.close()
    this.generation += 1
    this.workerRevision = 0
  }

  accept(frame, resolveSeq) {
    if (frame.revision !== this.workerRevision + 1) throw new Error('worker assistant stream revision gap')
    this.workerRevision = frame.revision
    const attemptId = `${this.generation}:${frame.attemptId}`
    if (frame.type === 'start') {
      if (this.active) throw new Error('overlapping worker assistant attempts')
      this.active = { attemptId, index: 0 }
    } else {
      if (this.active?.attemptId !== attemptId || frame.index !== this.active.index) {
        throw new Error('worker assistant stream attempt mismatch')
      }
      if (frame.type === 'chunk') this.active.index += 1
      else if (frame.type !== 'end') throw new Error('unknown worker assistant stream frame')
    }
    let outcome = frame.outcome
    if (frame.type === 'end' && outcome.kind === 'committed') {
      const seq = resolveSeq(outcome.seq)
      if (seq === undefined) throw new Error('worker assistant settlement was not mirrored')
      outcome = { ...outcome, seq }
    }
    this.publish({
      ...frame, attemptId, revision: ++this.revision,
      ...(outcome === undefined ? {} : { outcome }),
    })
    if (frame.type === 'end') this.active = undefined
  }

  close() {
    if (!this.active) return
    this.publish({
      type: 'end', ...this.active, revision: ++this.revision,
      outcome: { kind: 'abandoned' },
    })
    this.active = undefined
  }
}
