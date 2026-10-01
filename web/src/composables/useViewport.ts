/**
 * The viewport, as the shell needs to see it.
 *
 * CSS handles most of the responsive work on its own, but three things cannot
 * be said in a stylesheet: which of the three layouts is active (a `v-if` needs
 * a boolean, not a media query), whether the primary pointer is a finger, and
 * how wide the window is when a component has to compute a number from it.
 *
 * The switch is width, never the user agent. A narrow desktop window therefore
 * behaves exactly like a phone, and a wide phone in landscape gets the desktop
 * layout — which is the point, because the alternative is a page that lies
 * about its own size.
 *
 * The breakpoints here are the same contract `styles.css` publishes:
 *
 *   phone    width <= 640px
 *   tablet   641px .. 1024px
 *   desktop  width > 1024px
 *
 * The environment is injectable — the same shape of seam `i18n/useLocale.ts`
 * uses — because the tests run in the `node` environment, with no `window` and
 * no `matchMedia` to stub.
 */

import { computed, getCurrentScope, onScopeDispose, ref, type ComputedRef, type Ref } from 'vue';

export type Viewport = 'phone' | 'tablet' | 'desktop';

/** Last width that still counts as a phone. */
export const PHONE_MAX_WIDTH = 640;

/** Last width that still counts as a tablet. */
export const TABLET_MAX_WIDTH = 1024;

/**
 * What "touch" means: the primary pointer cannot hover and is coarse.
 *
 * A media query, deliberately — a user agent string is a guess about hardware
 * that a convertible laptop, a stylus and a Bluetooth mouse all defeat.
 */
export const TOUCH_MEDIA_QUERY = '(hover: none) and (pointer: coarse)';

/**
 * The width assumed when there is no `window` at all.
 *
 * A module imported in a test or a prerender pass still gets an answer, and the
 * widest layout is the safe one to assume.
 */
export const DOMLESS_WIDTH = 1280;

/** Which layout a viewport of `width` CSS pixels gets. */
export function viewportForWidth(width: number): Viewport {
  if (width <= PHONE_MAX_WIDTH) return 'phone';
  if (width <= TABLET_MAX_WIDTH) return 'tablet';
  return 'desktop';
}

/**
 * The globals {@link useViewport} reads, injectable so it runs headless.
 *
 * `onViewportChange` is the one subscription the composable makes; it fires for
 * anything that could change either answer — a resize, a rotation, or the
 * pointer media query flipping — and its return value detaches it.
 */
export interface ViewportEnvironment {
  /** The current viewport width, in CSS pixels. */
  width(): number;
  /** Whether the primary pointer is a coarse one that cannot hover. */
  isTouch(): boolean;
  /** Calls `handler` when the width or the pointer could have changed. */
  onViewportChange(handler: () => void): () => void;
}

/**
 * The real browser globals.
 *
 * The `resize` stream is **debounced**, not rAF-throttled: a resize drag emits
 * events faster than a frame, and the trailing edge is the only one whose width
 * anyone cares about. The debounce lives here rather than in
 * {@link useViewport} so that a test can drive the subscription synchronously
 * instead of advancing fake timers.
 */
export function browserViewportEnvironment(options: { debounceMs?: number } = {}): ViewportEnvironment {
  if (typeof window === 'undefined') {
    return {
      width: () => DOMLESS_WIDTH,
      isTouch: () => false,
      onViewportChange: () => () => {},
    };
  }

  const debounceMs = options.debounceMs ?? 100;

  return {
    width: () => window.innerWidth,
    isTouch: () => window.matchMedia(TOUCH_MEDIA_QUERY).matches,
    onViewportChange(handler: () => void): () => void {
      let timer: ReturnType<typeof setTimeout> | null = null;

      const fire = (): void => {
        timer = null;
        handler();
      };

      const schedule = (): void => {
        if (debounceMs <= 0) {
          fire();
          return;
        }
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(fire, debounceMs);
      };

      const coarse = window.matchMedia(TOUCH_MEDIA_QUERY);

      window.addEventListener('resize', schedule);
      // A rotation reports its final width late on iOS, so it gets its own
      // signal rather than being left to the resize that may not arrive.
      window.addEventListener('orientationchange', schedule);
      coarse.addEventListener('change', schedule);

      return () => {
        if (timer !== null) {
          clearTimeout(timer);
          timer = null;
        }
        window.removeEventListener('resize', schedule);
        window.removeEventListener('orientationchange', schedule);
        coarse.removeEventListener('change', schedule);
      };
    },
  };
}

/**
 * The live viewport.
 *
 * `width` is the reactive source; `viewport` and `isPhone` are derived from it,
 * so there is one number to keep true. `isTouch` is read from the pointer media
 * query and is deliberately *not* derived from the width: a touch laptop at
 * 1280px is still a touch device, and a narrow desktop window is still a mouse.
 *
 * Call it from a component's `setup` and the subscription is torn down with the
 * component. Called outside a scope — as a test does — nothing is registered to
 * dispose, and the returned refs stay usable.
 */
export function useViewport(env: ViewportEnvironment = browserViewportEnvironment()): {
  width: Ref<number>;
  viewport: ComputedRef<Viewport>;
  isPhone: ComputedRef<boolean>;
  isTouch: ComputedRef<boolean>;
} {
  const width = ref(env.width());
  const touch = ref(env.isTouch());

  const stop = env.onViewportChange(() => {
    width.value = env.width();
    touch.value = env.isTouch();
  });

  if (getCurrentScope()) {
    onScopeDispose(stop);
  }

  return {
    width,
    viewport: computed(() => viewportForWidth(width.value)),
    isPhone: computed(() => width.value <= PHONE_MAX_WIDTH),
    isTouch: computed(() => touch.value),
  };
}
