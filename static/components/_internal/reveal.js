/**
 * Bring a form field with an error into view.
 *
 * A field can sit where the editor cannot see it: in an inactive tab, a
 * collapsed group or collapsible, or a collapsed array/blocks row. An error
 * rendered there is invisible, and the form looks like it refused to save
 * for no reason. {@link revealField} opens every such ancestor; the pre-submit
 * validation calls it for the errors it draws, and the edit form calls it on
 * load for the errors the server re-render drew.
 *
 * A field hidden by its display condition is not revealed — it is hidden by
 * the rules, not by layout; {@link isConditionHidden} tells the caller, who
 * reports its error elsewhere.
 *
 * @module reveal
 * @stability internal
 */

/** The collapsed modifier of each collapsible container class. */
const COLLAPSED = {
  form__group: 'form__group--collapsed',
  form__collapsible: 'form__collapsible--collapsed',
};

/**
 * Whether `wrapper` (a `[data-field-name]` field wrapper) or one of its
 * ancestors is hidden by a display condition.
 *
 * @param {Element} wrapper
 * @returns {boolean}
 */
export function isConditionHidden(wrapper) {
  return wrapper.closest('.form__field--hidden') !== null;
}

/**
 * The tab button of the tab panel `panel` belongs to, or `null`.
 *
 * @param {Element} panel A `.form__tabs-panel`.
 * @returns {HTMLElement|null}
 */
export function tabButtonOf(panel) {
  const tabs = panel.closest('crap-tabs');
  const index = /** @type {HTMLElement} */ (panel).dataset.tabPanel;
  if (!tabs || index === undefined) return null;

  const buttons = /** @type {HTMLElement[]} */ ([
    ...tabs.querySelectorAll(`[data-action="switch-tab"][data-tab-index="${index}"]`),
  ]);
  return buttons.find((b) => b.closest('crap-tabs') === tabs) ?? null;
}

/**
 * Expand one collapsed container: a group, a collapsible, or an array row.
 *
 * @param {Element} el
 */
function expand(el) {
  if (el.matches('.form__array-row--collapsed')) {
    el.classList.remove('form__array-row--collapsed');
    el.querySelector('.form__array-row-toggle')?.setAttribute('aria-expanded', 'true');
    return;
  }

  for (const [base, collapsed] of Object.entries(COLLAPSED)) {
    if (!el.matches(`.${base}.${collapsed}`)) continue;
    el.classList.remove(collapsed);

    const toggle = [...el.querySelectorAll('[data-action="toggle-group"]')].find(
      (btn) => btn.closest('[data-collapsible]') === el,
    );
    toggle?.setAttribute('aria-expanded', 'true');
  }
}

/**
 * Open every ancestor of `wrapper` that hides it: expand collapsed groups,
 * collapsibles and array rows, and switch each enclosing tab group to the
 * tab that holds it (outermost first, so a nested tab group is reached).
 *
 * @param {Element} wrapper
 */
export function revealField(wrapper) {
  /** @type {Element[]} */
  const chain = [];
  for (let el = wrapper.parentElement; el; el = el.parentElement) chain.unshift(el);

  for (const el of chain) {
    if (el.matches('.form__tabs-panel.form__tabs-panel--hidden')) {
      tabButtonOf(el)?.click();
      continue;
    }
    expand(el);
  }
}
