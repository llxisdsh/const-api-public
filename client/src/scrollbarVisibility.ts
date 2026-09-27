const SCROLLBAR_OPACITY_PROPERTY = "--app-scrollbar-opacity";
const DEFAULT_HOLD_MS = 650;
const DEFAULT_FADE_MS = 480;

type ScrollbarVisibilityTiming = {
  holdMs?: number;
  fadeMs?: number;
};

type ScrollbarState = {
  holdTimer?: number;
  animationFrame?: number;
};

function scrollbarOwner(target: EventTarget | null, documentTarget: Document) {
  if (target === documentTarget) {
    return documentTarget.scrollingElement ?? documentTarget.documentElement;
  }

  const ElementConstructor = documentTarget.defaultView?.Element;
  return ElementConstructor && target instanceof ElementConstructor ? target : null;
}

function ownerStyle(owner: Element) {
  return owner instanceof HTMLElement || owner instanceof SVGElement ? owner.style : null;
}

function setScrollbarOpacity(owner: Element, opacity: number) {
  const percentage = `${Math.max(0, Math.min(1, opacity)) * 100}%`;
  ownerStyle(owner)?.setProperty(SCROLLBAR_OPACITY_PROPERTY, percentage);
}

function clearScrollbarOpacity(owner: Element) {
  ownerStyle(owner)?.removeProperty(SCROLLBAR_OPACITY_PROPERTY);
}

export function installAutoHidingScrollbars(
  documentTarget: Document = document,
  timing: ScrollbarVisibilityTiming = {},
) {
  // WKWebView already provides the native macOS overlay and fade behavior.
  if (documentTarget.documentElement.dataset.platform === "macos") {
    return () => {};
  }

  const windowTarget = documentTarget.defaultView;
  if (!windowTarget) return () => {};

  const holdMs = timing.holdMs ?? DEFAULT_HOLD_MS;
  const fadeMs = timing.fadeMs ?? DEFAULT_FADE_MS;
  const reduceMotion = windowTarget.matchMedia?.("(prefers-reduced-motion: reduce)").matches
    ?? false;
  const states = new Map<Element, ScrollbarState>();
  const requestFrame = windowTarget.requestAnimationFrame?.bind(windowTarget)
    ?? ((callback: FrameRequestCallback) => windowTarget.setTimeout(
      () => callback(windowTarget.performance.now()),
      16,
    ));
  const cancelFrame = windowTarget.cancelAnimationFrame?.bind(windowTarget)
    ?? windowTarget.clearTimeout.bind(windowTarget);

  const cancelState = (owner: Element) => {
    const state = states.get(owner);
    if (!state) return;
    if (state.holdTimer !== undefined) {
      windowTarget.clearTimeout(state.holdTimer);
    }
    if (state.animationFrame !== undefined) {
      cancelFrame(state.animationFrame);
    }
    states.delete(owner);
  };

  const hideScrollbar = (owner: Element) => {
    clearScrollbarOpacity(owner);
    states.delete(owner);
  };

  const startFade = (owner: Element, state: ScrollbarState) => {
    if (reduceMotion || fadeMs <= 0) {
      hideScrollbar(owner);
      return;
    }

    let startedAt: number | undefined;
    const renderFadeFrame = (timestamp: number) => {
      if (!states.has(owner)) return;
      startedAt ??= timestamp;
      const progress = Math.min(1, (timestamp - startedAt) / fadeMs);
      const easedProgress = progress * progress * (3 - 2 * progress);
      setScrollbarOpacity(owner, 1 - easedProgress);

      if (progress < 1) {
        state.animationFrame = requestFrame(renderFadeFrame);
      } else {
        hideScrollbar(owner);
      }
    };

    state.animationFrame = requestFrame(renderFadeFrame);
  };

  const revealScrollbar = (owner: Element) => {
    cancelState(owner);
    setScrollbarOpacity(owner, 1);
    const state: ScrollbarState = {};
    state.holdTimer = windowTarget.setTimeout(() => {
      state.holdTimer = undefined;
      startFade(owner, state);
    }, holdMs);
    states.set(owner, state);
  };

  const revealActiveScrollbar = (event: Event) => {
    const owner = scrollbarOwner(event.target, documentTarget);
    if (owner) revealScrollbar(owner);
  };

  const revealHoveredHorizontalScrollbar = (event: Event) => {
    let owner = scrollbarOwner(event.target, documentTarget);
    while (owner) {
      if (owner.scrollWidth > owner.clientWidth) {
        const overflowX = windowTarget.getComputedStyle(owner).overflowX;
        if (overflowX === "auto" || overflowX === "scroll" || overflowX === "overlay") {
          revealScrollbar(owner);
          return;
        }
      }
      owner = owner.parentElement;
    }
  };

  documentTarget.addEventListener("scroll", revealActiveScrollbar, true);
  documentTarget.addEventListener("pointerover", revealHoveredHorizontalScrollbar, true);
  documentTarget.addEventListener("pointermove", revealHoveredHorizontalScrollbar, true);
  return () => {
    documentTarget.removeEventListener("scroll", revealActiveScrollbar, true);
    documentTarget.removeEventListener("pointerover", revealHoveredHorizontalScrollbar, true);
    documentTarget.removeEventListener("pointermove", revealHoveredHorizontalScrollbar, true);
    for (const owner of Array.from(states.keys())) {
      cancelState(owner);
      clearScrollbarOpacity(owner);
    }
  };
}
