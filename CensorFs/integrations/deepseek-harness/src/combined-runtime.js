/**
 * Mode boundary between the two exploration tracks.
 *
 * - FUSE track (`branch_explore`): complete child Harness processes inside
 *   their own mount namespaces, driven through the dsh-jsonrpc-agent adapter.
 * - In-process track (`branch_explore_inprocess`): Agents inside this process
 *   whose file and shell tools execute in dedicated mount-namespace Runners.
 *
 * The two runtimes never import each other; this class is the ONLY module that
 * knows both. Track availability comes from config (`inProcessOnly` /
 * `fuseOnly`): a disabled track is passed as `undefined` and every
 * cross-track call fails with an explicit, track-naming error instead of
 * silently misrouting into the other runtime.
 */
export class CombinedBranchExploreRuntime {
  constructor(fuseRuntime, namespaceRuntime, config) {
    this.config = config
    this.fuseRuntime = fuseRuntime
    this.namespaceRuntime = namespaceRuntime
    if (config.inProcessOnly === true) {
      if (fuseRuntime !== undefined) throw new Error('combined explore runtime mis-wired: FUSE runtime constructed while inProcessOnly=true')
    } else if (fuseRuntime === undefined) {
      throw new Error('combined explore runtime mis-wired: FUSE track enabled but no FUSE runtime was constructed')
    }
    if (config.fuseOnly === true) {
      if (namespaceRuntime !== undefined) throw new Error('combined explore runtime mis-wired: runner runtime constructed while fuseOnly=true')
    } else if (namespaceRuntime === undefined) {
      throw new Error('combined explore runtime mis-wired: runner track enabled but no runner runtime was constructed')
    }
  }

  #requireFuse() {
    if (this.fuseRuntime === undefined) {
      throw new Error('this operation needs the FUSE track (branch_explore), which is disabled in this deployment (fuseOnly=true or inProcessOnly=true); use branch_explore_inprocess instead')
    }
    return this.fuseRuntime
  }

  #requireRunner() {
    if (this.namespaceRuntime === undefined) {
      throw new Error('this operation needs the in-process track (branch_explore_inprocess), which is disabled in this deployment (fuseOnly=true); use branch_explore instead')
    }
    return this.namespaceRuntime
  }

  /** The recorded exploration mode of `runId`, or `undefined` when unknown. */
  #runMode(agent, runId) {
    const started = agent.session.events.find((event) => event.type === 'exploration-started' && event.data?.runId === runId)
    return started?.data?.mode
  }

  /** Route to the runtime that owns `runId`; fails closed on cross-track calls. */
  #runtimeForRun(agent, runId) {
    const mode = this.#runMode(agent, runId)
    if (mode === 'in-process') return { mode, runtime: this.#requireRunner() }
    return { mode, runtime: this.#requireFuse() }
  }

  explore(agent, input, signal) {
    if (this.config.inProcessOnly) {
      throw new Error('branch_explore is unavailable: this deployment is in-process only (inProcessOnly=true); use branch_explore_inprocess')
    }
    const fuseRuntime = this.#requireFuse()
    if (this.config.childCommand === undefined) {
      throw new Error('branch_explore requires config.childCommand (the external worker command); set DSH_CENSORFS_CHILD_COMMAND or use branch_explore_inprocess')
    }
    return fuseRuntime.explore(agent, input, signal)
  }

  exploreInProcess(agent, input, signal) {
    return this.#requireRunner().exploreInProcess(agent, input, signal)
  }

  doctor() {
    return this.#requireRunner().doctor()
  }

  state(agent, runId) {
    return this.#runtimeForRun(agent, runId).runtime.state(agent, runId)
  }

  validate(agent, runId, variantId, profile) {
    return this.#runtimeForRun(agent, runId).runtime.validate(agent, runId, variantId, profile)
  }

  publish(agent, runId, variantId, force) {
    return this.#runtimeForRun(agent, runId).runtime.publish(agent, runId, variantId, force)
  }

  abort(agent, runId, variantId) {
    return this.#runtimeForRun(agent, runId).runtime.abort(agent, runId, variantId)
  }

  preview(agent, runId, variantId) {
    return this.#runtimeForRun(agent, runId).runtime.preview(agent, runId, variantId)
  }

  updatePolicy(agent, runId, variantId, directive) {
    const mode = this.#runMode(agent, runId)
    if (mode === 'in-process') {
      throw new Error('/censorfs-policy is unavailable for in-process runs; use branch_explore (FUSE mode)')
    }
    return this.#requireFuse().updatePolicy(agent, runId, variantId, directive)
  }

  resolveRootSession(sessionId) {
    return this.#requireFuse().resolveRootSession(sessionId)
  }

  clearSubagentGraph(sessionId) {
    return this.#requireFuse().clearSubagentGraph(sessionId)
  }

  subagentTree() {
    return this.#requireFuse().subagentTree()
  }

  subagentOverview() {
    return this.#requireFuse().subagentOverview()
  }

  injectSysActivity(sessionId, activity) {
    return this.#requireFuse().injectSysActivity(sessionId, activity)
  }

  trackSubagentStart(info) {
    this.fuseRuntime?.trackSubagentStart(info)
    this.namespaceRuntime?.trackSubagentStart(info)
  }

  trackSubagentEnd(info) {
    this.fuseRuntime?.trackSubagentEnd(info)
    this.namespaceRuntime?.trackSubagentEnd(info)
  }

  trackSubagentActivity(session, event) {
    this.fuseRuntime?.trackSubagentActivity(session, event)
    this.namespaceRuntime?.trackSubagentActivity(session, event)
  }

  async dispose() {
    await Promise.all([
      this.fuseRuntime?.dispose(),
      this.namespaceRuntime?.dispose(),
    ].filter(Boolean))
  }
}
