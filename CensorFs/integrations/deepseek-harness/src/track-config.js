/**
 * Track availability policy — the single source of truth for which
 * exploration tracks a deployment enables.
 *
 * - FUSE track (`branch_explore`): external child Harness processes in their
 *   own mount namespaces.
 * - Runner track (`branch_explore_inprocess`): Agents in this process whose
 *   file/shell tools execute in dedicated mount-namespace Runners.
 *
 * Deliberately dependency-free (node builtins only) so the gating contract is
 * testable without the dsh peer packages.
 */

/**
 * @returns {{ fuse: boolean, runner: boolean }} enabled tracks; at least one.
 * @throws when both single-track flags are set (nothing would be enabled).
 */
export function resolveEnabledTracks(config) {
  const inProcessOnly = config?.inProcessOnly === true
  const fuseOnly = config?.fuseOnly === true
  if (inProcessOnly && fuseOnly) {
    throw new TypeError('dsh-branch-explore config: inProcessOnly and fuseOnly are mutually exclusive; enable at least one exploration track')
  }
  return {
    fuse: !inProcessOnly,
    runner: !fuseOnly,
  }
}
