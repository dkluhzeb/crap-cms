/**
 * Form UI state preservation across HTMX swaps.
 *
 * Snapshots scroll position, active tab indices, and group/collapsible
 * collapsed states before form submissions (POST/PUT). Restores them
 * on the next page load — either via htmx:afterSettle (in-place swap)
 * or DOMContentLoaded (full page reload via HX-Redirect).
 *
 * URL-scoped: only restores if the loaded page matches the saved URL,
 * so state persists across same-page saves but resets on navigation.
 */

const STORAGE_KEY = 'crap-form-state';

/**
 * Snapshot all transient form UI state into sessionStorage.
 */
function saveFormState() {
  /** @type {{ url: string, scrollY: number, tabs: Object<string,string>, groups: Object<string,string> }} */
  const state = {
    url: location.pathname,
    scrollY: window.scrollY,
    tabs: {},
    groups: {},
  };

  // Active tab indices
  document.querySelectorAll('.form__tabs[data-tabs-name]').forEach(
    /** @param {HTMLElement} tabs */ (tabs) => {
      const name = tabs.getAttribute('data-tabs-name');
      const active = tabs.querySelector('.form__tabs-tab--active');
      if (name && active) {
        state.tabs[name] = active.getAttribute('data-tab-index');
      }
    }
  );

  // Group/collapsible collapsed states
  document.querySelectorAll('[data-collapsible][data-group-name]').forEach(
    /** @param {HTMLElement} fieldset */ (fieldset) => {
      const name = fieldset.getAttribute('data-group-name');
      if (!name) return;
      const cls = fieldset.classList.contains('form__collapsible')
        ? 'form__collapsible--collapsed'
        : 'form__group--collapsed';
      state.groups[name] = fieldset.classList.contains(cls) ? '1' : '0';
    }
  );

  sessionStorage.setItem(STORAGE_KEY, JSON.stringify(state));
}

/**
 * Restore form UI state from sessionStorage if on the same page.
 */
function restoreFormState() {
  const raw = sessionStorage.getItem(STORAGE_KEY);
  sessionStorage.removeItem(STORAGE_KEY);
  if (!raw) return;

  /** @type {{ url: string, scrollY: number, tabs: Object<string,string>, groups: Object<string,string> }} */
  let state;
  try { state = JSON.parse(raw); } catch { return; }
  if (state.url !== location.pathname) return;

  // Restore tabs
  for (const [name, index] of Object.entries(state.tabs)) {
    const tabs = document.querySelector(`.form__tabs[data-tabs-name="${name}"]`);
    if (!tabs) continue;
    const btn = tabs.querySelector(`.form__tabs-tab[data-tab-index="${index}"]`);
    const panel = tabs.querySelector(`[data-tab-panel="${index}"]`);
    if (!btn || !panel) continue;

    tabs.querySelectorAll('.form__tabs-tab').forEach(t => {
      t.classList.remove('form__tabs-tab--active');
      t.setAttribute('aria-selected', 'false');
    });
    tabs.querySelectorAll('.form__tabs-panel').forEach(p => p.classList.add('form__tabs-panel--hidden'));
    btn.classList.add('form__tabs-tab--active');
    btn.setAttribute('aria-selected', 'true');
    panel.classList.remove('form__tabs-panel--hidden');
  }

  // Restore groups
  for (const [name, val] of Object.entries(state.groups)) {
    const fieldset = document.querySelector(`[data-collapsible][data-group-name="${name}"]`);
    if (!fieldset) continue;
    const cls = fieldset.classList.contains('form__collapsible')
      ? 'form__collapsible--collapsed'
      : 'form__group--collapsed';
    if (val === '1') {
      fieldset.classList.add(cls);
    } else {
      fieldset.classList.remove(cls);
    }
  }

  // Restore scroll (after DOM updates so layout is settled)
  if (state.scrollY != null) {
    requestAnimationFrame(() => {
      window.scrollTo(0, state.scrollY);
    });
  }
}

/**
 * @param {CustomEvent} e
 */
function onBeforeRequest(e) {
  if (e.detail.requestConfig.verb === 'get') return;
  saveFormState();
}

document.addEventListener('htmx:beforeRequest', onBeforeRequest);
document.addEventListener('htmx:afterSettle', restoreFormState);
document.addEventListener('DOMContentLoaded', restoreFormState);
