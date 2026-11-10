import { z } from 'zod'

const stateSchema = z.object({
  'next-turn': z.array(z.custom()).readonly(),
  'next-step': z.array(z.custom()).readonly(),
}).readonly()

/** Public inbox wire contract; the native loop's definition is not exported. */
export const inboxProjectionDefinition = {
  key: 'inbox', stateVersion: 1, stateSchema,
  init: () => ({ 'next-turn': [], 'next-step': [] }),
  apply(state, event) {
    if (event.type !== 'agent/inbox/spliced') return state
    const { target, start, removedCount = 0, inserted } = event.data
    const messages = state[target]
    if (!messages || !Number.isSafeInteger(start) || start < 0 || start > messages.length
      || !Number.isSafeInteger(removedCount) || removedCount < 0 || start + removedCount > messages.length) {
      throw new Error(`invalid inbox splice at seq ${event.seq}`)
    }
    const next = { ...state, [target]: messages.toSpliced(start, removedCount, ...inserted) }
    const ids = [...next['next-turn'], ...next['next-step']].map(message => message.id)
    if (new Set(ids).size !== ids.length) throw new Error(`duplicate pending message at seq ${event.seq}`)
    return next
  },
  wire: { viewSchema: stateSchema, view: state => state },
}

/** Read mirrored durable state and route identity-based queue edits to its author. */
export class ProxyInbox {
  constructor(projections, session, submit) {
    this.projections = projections
    this.session = session
    this.submit = submit
  }

  get nextTurn() { return this.current()['next-turn'] }
  get nextStep() { return this.current()['next-step'] }
  get hasPending() { return this.nextTurn.length > 0 || this.nextStep.length > 0 }
  current() {
    const state = this.projections.stateOf(this.session, 'inbox')
    if (!state) throw new Error('agentcensor: inbox projection is unavailable')
    return state
  }
  locate(id) { return [...this.nextTurn, ...this.nextStep].some(message => message.id === id) }
  clear() { this.submit({ action: 'clear' }) }
  append(target, message) { this.submit({ action: 'append', target, message }) }
  prepend(target, message) { this.submit({ action: 'prepend', target, message }) }
  replace(id, message) {
    if (!this.locate(id)) return false
    this.submit({ action: 'replace', id, message })
    return true
  }
  remove(id) {
    if (!this.locate(id)) return false
    this.submit({ action: 'remove', id })
    return true
  }
  splice(target, start, deleteCount, inserted) {
    const values = [...this.current()[target]]
    const removed = values.splice(start, deleteCount, ...inserted)
    const normalizedStart = start === Infinity ? values.length - inserted.length : start
    this.submit({ action: 'splice', target, start: normalizedStart,
      deleteCount: deleteCount === Infinity ? removed.length : deleteCount, inserted })
    return removed
  }
}

export function applyInboxCommand(inbox, command) {
  switch (command.action) {
    case 'clear': return inbox.clear()
    case 'append': return inbox.append(command.target, command.message)
    case 'prepend': return inbox.prepend(command.target, command.message)
    case 'replace': return inbox.replace(command.id, command.message)
    case 'remove': return inbox.remove(command.id)
    case 'splice': return inbox.splice(command.target, command.start, command.deleteCount, command.inserted)
    default: throw new Error('invalid worker inbox command')
  }
}
