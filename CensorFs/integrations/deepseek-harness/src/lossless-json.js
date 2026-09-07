export function losslessJson(value) {
  return JSON.parse(JSON.stringify(value))
}
