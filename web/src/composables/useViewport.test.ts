import { effectScope } from 'vue';
import { describe, expect, it } from 'vitest';

import {
  DOMLESS_WIDTH,
  PHONE_MAX_WIDTH,
  TABLET_MAX_WIDTH,
  TOUCH_MEDIA_QUERY,
  browserViewportEnvironment,
  useViewport,
  viewportForWidth,
  type ViewportEnvironment,
} from './useViewport';

/**
 * A window that does not exist: a width the test sets, a pointer answer the
 * test sets, and a subscriber list the test can fire by hand.
 */
function fakeViewport(initialWidth: number, options: { touch?: boolean } = {}) {
  let width = initialWidth;
  let touch = options.touch ?? false;
  const listeners = new Set<() => void>();
  let subscribes = 0;
  let unsubscribes = 0;

  const env: ViewportEnvironment = {
    width: () => width,
    isTouch: () => touch,
    onViewportChange(handler) {
      subscribes += 1;
      listeners.add(handler);
      return () => {
        unsubscribes += 1;
        listeners.delete(handler);
      };
    },
  };

  return {
    env,
    setWidth(next: number) {
      width = next;
      for (const handler of [...listeners]) handler();
    },
    setTouch(next: boolean) {
      touch = next;
      for (const handler of [...listeners]) handler();
    },
    subscribes: () => subscribes,
    unsubscribes: () => unsubscribes,
    listeners: () => listeners.size,
  };
}

describe('viewportForWidth', () => {
  it('calls the last phone width a phone', () => {
    expect(viewportForWidth(PHONE_MAX_WIDTH)).toBe('phone');
    expect(viewportForWidth(640)).toBe('phone');
  });

  it('calls the first width past it a tablet', () => {
    expect(viewportForWidth(PHONE_MAX_WIDTH + 1)).toBe('tablet');
    expect(viewportForWidth(641)).toBe('tablet');
  });

  it('calls the last tablet width a tablet', () => {
    expect(viewportForWidth(TABLET_MAX_WIDTH)).toBe('tablet');
    expect(viewportForWidth(1024)).toBe('tablet');
  });

  it('calls the first width past it a desktop', () => {
    expect(viewportForWidth(TABLET_MAX_WIDTH + 1)).toBe('desktop');
    expect(viewportForWidth(1025)).toBe('desktop');
  });

  it('covers the widths a phone and a monitor actually report', () => {
    expect(viewportForWidth(360)).toBe('phone');
    expect(viewportForWidth(390)).toBe('phone');
    expect(viewportForWidth(768)).toBe('tablet');
    expect(viewportForWidth(1280)).toBe('desktop');
    expect(viewportForWidth(2560)).toBe('desktop');
  });

  it('publishes the numbers styles.css also documents', () => {
    expect(PHONE_MAX_WIDTH).toBe(640);
    expect(TABLET_MAX_WIDTH).toBe(1024);
  });
});

describe('useViewport', () => {
  it('starts from the environment it was given', () => {
    const { env } = fakeViewport(500);

    const { width, viewport, isPhone, isTouch } = useViewport(env);

    expect(width.value).toBe(500);
    expect(viewport.value).toBe('phone');
    expect(isPhone.value).toBe(true);
    expect(isTouch.value).toBe(false);
  });

  it('recomputes every boundary when the width changes', () => {
    const fake = fakeViewport(360);
    const { viewport, isPhone } = useViewport(fake.env);

    expect(viewport.value).toBe('phone');

    fake.setWidth(641);
    expect(viewport.value).toBe('tablet');
    expect(isPhone.value).toBe(false);

    fake.setWidth(1025);
    expect(viewport.value).toBe('desktop');

    fake.setWidth(640);
    expect(viewport.value).toBe('phone');
    expect(isPhone.value).toBe(true);
  });

  it('follows the pointer media query rather than the width', () => {
    const fake = fakeViewport(1280, { touch: true });
    const { isTouch, viewport } = useViewport(fake.env);

    expect(viewport.value).toBe('desktop');
    expect(isTouch.value).toBe(true);

    fake.setTouch(false);
    expect(isTouch.value).toBe(false);
  });

  it('subscribes once and stops when its scope is disposed', () => {
    const fake = fakeViewport(360);
    const scope = effectScope();
    const { viewport } = scope.run(() => useViewport(fake.env))!;

    expect(fake.subscribes()).toBe(1);
    expect(fake.listeners()).toBe(1);

    scope.stop();

    expect(fake.unsubscribes()).toBe(1);
    expect(fake.listeners()).toBe(0);

    // Nothing is listening any more, so the layout it last saw stands.
    fake.setWidth(1280);
    expect(viewport.value).toBe('phone');
  });

  it('uses the coarse-pointer media query, never a user agent string', () => {
    expect(TOUCH_MEDIA_QUERY).toBe('(hover: none) and (pointer: coarse)');
  });
});

describe('browserViewportEnvironment', () => {
  it('answers without a window, assuming the widest layout', () => {
    const env = browserViewportEnvironment();

    expect(env.width()).toBe(DOMLESS_WIDTH);
    expect(env.isTouch()).toBe(false);
    expect(viewportForWidth(env.width())).toBe('desktop');
    // Detaching a subscription that was never made is still a no-op, not a throw.
    expect(() => env.onViewportChange(() => {})()).not.toThrow();
  });

  it('is what useViewport falls back to, and it boots headless', () => {
    const { width, viewport, isTouch } = useViewport();

    expect(width.value).toBe(DOMLESS_WIDTH);
    expect(viewport.value).toBe('desktop');
    expect(isTouch.value).toBe(false);
  });
});
