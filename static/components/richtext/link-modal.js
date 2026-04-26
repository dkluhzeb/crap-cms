/**
 * Link insert/edit modal for `<crap-richtext>`.
 *
 * @module richtext/link-modal
 */

import { h } from '../h.js';
import { t } from '../i18n.js';

/** Allowed protocols for inserted links — blocks `javascript:` etc. */
const ALLOWED_LINK_PROTOS = new Set(['http', 'https', 'mailto', 'tel', '']);

/**
 * Open the link modal inside the richtext host's shadow root.
 * Replaces any existing `.crap-node-modal` first.
 *
 * @param {{ shadowRoot: ShadowRoot|null, _view: any }} host
 * @param {any} schema
 * @param {Record<string, any>} attrs Current link attrs (empty for insert mode).
 */
export function openLinkModal(host, schema, attrs) {
  if (!host.shadowRoot) return;
  host.shadowRoot.querySelector('.crap-node-modal')?.remove();

  const isEdit = !!attrs.href;
  const savedSelection = host._view.state.selection;
  const modal = buildLinkModal(attrs, isEdit);
  host.shadowRoot.appendChild(modal);
  modal.showModal();

  /** @type {HTMLInputElement|null} */
  const hrefInput = modal.querySelector('[data-field="href"]');
  hrefInput?.focus();
  wireLinkModal(host, modal, schema, attrs, savedSelection, isEdit);
}

/**
 * Whether `href` uses an allowlisted protocol. Schemeless URLs (no `:`)
 * are accepted.
 *
 * @param {string} href
 */
function isAllowedLinkProto(href) {
  if (!href.includes(':')) return true;
  const proto = href.split(':')[0].toLowerCase().trim();
  return ALLOWED_LINK_PROTOS.has(proto);
}

/**
 * @param {Record<string, any>} attrs
 * @param {boolean} isEdit
 */
function buildLinkModal(attrs, isEdit) {
  const footerButtons = [
    isEdit &&
      h('button', {
        type: 'button',
        class: ['crap-node-modal__btn', 'crap-node-modal__btn--danger'],
        text: t('remove_link'),
      }),
    h('button', {
      type: 'button',
      class: ['crap-node-modal__btn', 'crap-node-modal__btn--cancel'],
      text: t('cancel'),
    }),
    h('button', {
      type: 'button',
      class: ['crap-node-modal__btn', 'crap-node-modal__btn--ok'],
      text: t('apply'),
    }),
  ];

  return h(
    'dialog',
    {
      class: 'crap-node-modal',
      'aria-labelledby': 'crap-link-modal-heading',
    },
    h(
      'div',
      { class: 'crap-node-modal__dialog' },
      h('div', {
        class: 'crap-node-modal__header',
        id: 'crap-link-modal-heading',
        text: isEdit ? t('edit_link') : t('insert_link'),
      }),
      h(
        'div',
        { class: 'crap-node-modal__body' },
        labelledField(
          'crap-link-href',
          `${t('link_url')} *`,
          h('input', {
            type: 'url',
            class: 'crap-node-modal__input',
            id: 'crap-link-href',
            dataset: { field: 'href' },
            value: attrs.href || '',
            required: true,
          }),
        ),
        labelledField(
          'crap-link-title',
          t('link_title'),
          h('input', {
            type: 'text',
            class: 'crap-node-modal__input',
            id: 'crap-link-title',
            dataset: { field: 'title' },
            value: attrs.title || '',
          }),
        ),
        h(
          'div',
          { class: 'crap-node-modal__field' },
          h(
            'label',
            { class: 'crap-node-modal__checkbox' },
            h('input', {
              type: 'checkbox',
              dataset: { field: 'target' },
              checked: attrs.target === '_blank',
            }),
            ` ${t('link_open_new_tab')}`,
          ),
        ),
        h(
          'div',
          { class: 'crap-node-modal__field' },
          h(
            'label',
            { class: 'crap-node-modal__checkbox' },
            h('input', {
              type: 'checkbox',
              dataset: { field: 'rel' },
              checked: !!attrs.rel?.includes('nofollow'),
            }),
            ` ${t('link_nofollow')}`,
          ),
        ),
      ),
      h(
        'div',
        {
          class: ['crap-node-modal__footer', isEdit && 'crap-node-modal__footer--with-remove'],
        },
        ...footerButtons,
      ),
    ),
  );
}

/**
 * @param {string} forId
 * @param {string} label
 * @param {HTMLElement} input
 */
function labelledField(forId, label, input) {
  return h(
    'div',
    { class: 'crap-node-modal__field' },
    h('label', { class: 'crap-node-modal__label', for: forId, text: label }),
    input,
  );
}

/**
 * @param {{ _view: any }} host
 * @param {HTMLDialogElement} modal
 * @param {any} schema
 * @param {Record<string, any>} attrs
 * @param {any} savedSelection
 * @param {boolean} isEdit
 */
function wireLinkModal(host, modal, schema, attrs, savedSelection, isEdit) {
  const close = () => {
    modal.close();
    modal.remove();
  };
  const apply = () => applyLink(host, modal, schema, attrs, savedSelection, isEdit, close);
  const remove = () => removeLink(host, schema, savedSelection, close);

  modal.addEventListener('cancel', (e) => {
    e.preventDefault();
    close();
  });
  modal.querySelector('.crap-node-modal__btn--cancel')?.addEventListener('click', close);
  modal.querySelector('.crap-node-modal__btn--ok')?.addEventListener('click', apply);
  modal.querySelector('.crap-node-modal__btn--danger')?.addEventListener('click', remove);

  /** @type {HTMLInputElement|null} */
  const hrefInput = modal.querySelector('[data-field="href"]');
  hrefInput?.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      apply();
    }
  });
}

/**
 * @param {{ _view: any }} host
 * @param {HTMLDialogElement} modal
 * @param {any} schema
 * @param {Record<string, any>} attrs
 * @param {any} savedSelection
 * @param {boolean} isEdit
 * @param {() => void} close
 */
function applyLink(host, modal, schema, attrs, savedSelection, isEdit, close) {
  /** @type {HTMLInputElement|null} */
  const hrefEl = modal.querySelector('[data-field="href"]');
  const href = hrefEl?.value.trim() || '';
  if (!href || !isAllowedLinkProto(href)) return;

  /** @type {HTMLInputElement|null} */
  const titleEl = modal.querySelector('[data-field="title"]');
  /** @type {HTMLInputElement|null} */
  const targetEl = modal.querySelector('[data-field="target"]');
  /** @type {HTMLInputElement|null} */
  const relEl = modal.querySelector('[data-field="rel"]');

  const title = titleEl?.value.trim() || null;
  const target = targetEl?.checked ? '_blank' : null;
  // Preserve existing rel tokens (e.g. noopener, noreferrer); just toggle nofollow.
  const existingRel = (attrs.rel || '').split(/\s+/).filter(Boolean);
  const otherTokens = existingRel.filter((/** @type {string} */ tok) => tok !== 'nofollow');
  const relTokens = relEl?.checked ? ['nofollow', ...otherTokens] : otherTokens;
  const rel = relTokens.length > 0 ? relTokens.join(' ') : null;

  const view = host._view;
  const markType = schema.marks.link;
  let { tr } = view.state;
  tr = tr.setSelection(savedSelection);
  const { from, to } = savedSelection;
  if (isEdit) tr = tr.removeMark(from, to, markType);
  tr = tr.addMark(from, to, markType.create({ href, title, target, rel }));
  view.dispatch(tr);

  close();
  view.focus();
}

/**
 * @param {{ _view: any }} host
 * @param {any} schema
 * @param {any} savedSelection
 * @param {() => void} close
 */
function removeLink(host, schema, savedSelection, close) {
  const view = host._view;
  let { tr } = view.state;
  tr = tr.setSelection(savedSelection);
  const { from, to } = savedSelection;
  tr = tr.removeMark(from, to, schema.marks.link);
  view.dispatch(tr);
  close();
  view.focus();
}
