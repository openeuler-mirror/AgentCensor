// The native DSH headless runner prints a final summary only. A separate pipe
// carries approved live text to cpctl without duplicating that summary.
import { createWriteStream } from 'node:fs';

export function cliOutput(enabled) {
  let output, lineOpen = false;
  async function write(value) {
    if (!output) {
      output = createWriteStream(null, { fd: 3, autoClose: false });
      output.on('error', () => {}); // The write callback propagates a fixed error.
    }
    await new Promise((ok, no) => output.write(value, failure => failure ? no(new Error('cli_output_failed')) : ok()));
  }
  return async event => {
    if (!enabled) return;
    if (event.type === 'text') {
      await write(event.text);
      lineOpen = !event.text.endsWith('\n');
    } else if (event.type === 'finish' && lineOpen) {
      await write('\n');
      lineOpen = false;
    }
  };
}
