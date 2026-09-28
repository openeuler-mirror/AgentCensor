/** Marker plugin for the CensorPivot composite DSH layer. */
export const name = 'censorpivot'
export const inject = []

export function apply(ctx) {
  ctx.logger?.info?.('CensorPivot composite enabled: CensorFS + CensorGuard + CensorScope')
}

export default { name, inject, apply }
