// Navigation, backed by real session history.
//
// Android's back button and the left-swipe gesture both reach the webview:
// Tauri's activity calls `webView.goBack()` when `canGoBack()`, and otherwise
// finishes the activity. So the *only* thing that stops back from closing the
// app is having history entries to pop — which means every view the user can
// enter has to push one. Hiding a sheet with `hidden` alone never did.
//
// Each state also carries a distinct hash, so an entry is guaranteed even
// where two states would otherwise share a URL, and so views are linkable.

export type Tab = "list" | "map" | "diag";

export type View =
  | { view: "tab"; tab: Tab }
  | { view: "paper"; tab: Tab; id: string }
  | { view: "origin"; tab: Tab }
  | { view: "settings"; tab: Tab };

export function hashFor(s: View): string {
  switch (s.view) {
    case "paper":
      return `#p/${s.id}`;
    case "origin":
      return "#where";
    case "settings":
      return "#settings";
    case "tab":
      return s.tab === "list" ? "#list" : `#${s.tab}`;
  }
}

/** Parse a hash into a view, for deep links and cold starts. */
export function viewFromHash(hash: string, tab: Tab = "list"): View {
  const h = hash.replace(/^#/, "");
  if (h.startsWith("p/")) {
    const id = h.slice(2);
    return id ? { view: "paper", tab, id } : { view: "tab", tab };
  }
  if (h === "where") return { view: "origin", tab };
  if (h === "settings") return { view: "settings", tab };
  if (h === "map" || h === "diag") return { view: "tab", tab: h };
  return { view: "tab", tab: "list" };
}

export function sameView(a: View, b: View): boolean {
  if (a.view !== b.view || a.tab !== b.tab) return false;
  if (a.view === "paper" && b.view === "paper") return a.id === b.id;
  return true;
}

/**
 * A history-backed navigator.
 *
 * `render` is given the state to display and must not navigate; everything
 * that changes the view goes through `go`, `replace` or `back`.
 */
export class Nav {
  private currentState: View;

  constructor(
    private render: (s: View) => void,
    initial: View,
  ) {
    this.currentState = initial;
    history.replaceState(initial, "", hashFor(initial));
    window.addEventListener("popstate", (e) => {
      // A state-less entry means we landed on something we did not push.
      const s = (e.state as View | null) ?? viewFromHash(location.hash);
      this.currentState = s;
      this.render(s);
    });
  }

  get current(): View {
    return this.currentState;
  }

  /** Enter a new view, leaving the present one on the stack for back. */
  go(s: View): void {
    if (sameView(s, this.currentState)) return;
    this.currentState = s;
    history.pushState(s, "", hashFor(s));
    this.render(s);
  }

  /** Change the present view without adding an entry. */
  replace(s: View): void {
    this.currentState = s;
    history.replaceState(s, "", hashFor(s));
    this.render(s);
  }

  /**
   * Leave the present view.
   *
   * Goes through history so the stack stays honest — closing a sheet with its
   * own button and dismissing it with the back gesture end up in the same
   * place. At the root there is nothing to pop, and Android then closes the
   * app, which is the right behaviour there.
   */
  back(): void {
    history.back();
  }

  /** Re-render the present state, e.g. after the data underneath changed. */
  refresh(): void {
    this.render(this.currentState);
  }
}
