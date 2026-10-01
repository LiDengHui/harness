import { ref, type Ref } from 'vue';

/**
 * A shared clock for the live parts of the transcript.
 *
 * Tool cards show a duration while they run, which needs a tick that is not
 * driven by events. One interval for the whole page is enough; it starts the
 * first time a card asks for the time.
 */
const now = ref(Date.now());

let timer: ReturnType<typeof setInterval> | null = null;

export function useNow(intervalMs = 500): Ref<number> {
  if (timer === null) {
    timer = setInterval(() => {
      now.value = Date.now();
    }, intervalMs);
  }
  return now;
}
