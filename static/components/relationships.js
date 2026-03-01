/**
 * Relationship field "View" links and multi-select form serialization.
 *
 * View links: shows/hides the link based on the select's current value
 * and updates the href to point to the selected item's edit page.
 *
 * Multi-select fix: browsers submit `<select multiple>` as repeated keys
 * (e.g., name=a&name=b), but our server uses HashMap<String, String> which
 * only keeps the last value. This module intercepts form submit and replaces
 * each `<select multiple>` with a hidden input containing comma-separated values.
 */

/** @param {HTMLFormElement} form */
function serializeMultiSelects(form) {
  form.querySelectorAll('select[multiple]').forEach(
    /** @param {HTMLSelectElement} select */ (select) => {
      const values = Array.from(select.selectedOptions).map((o) => o.value);
      const hidden = document.createElement('input');
      hidden.type = 'hidden';
      hidden.name = select.name;
      hidden.value = values.join(',');
      hidden.setAttribute('data-multi-select-serialized', 'true');
      select.disabled = true;
      select.insertAdjacentElement('afterend', hidden);
    }
  );
}

/** @param {HTMLFormElement} form */
function restoreMultiSelects(form) {
  form.querySelectorAll('input[data-multi-select-serialized]').forEach(
    /** @param {HTMLInputElement} hidden */ (hidden) => {
      const select = hidden.previousElementSibling;
      if (select && select.tagName === 'SELECT') {
        select.disabled = false;
      }
      hidden.remove();
    }
  );
}

function initMultiSelectSerialization() {
  document.querySelectorAll('form').forEach(
    /** @param {HTMLFormElement} form */ (form) => {
      if (form.hasAttribute('data-multi-select-init')) return;
      form.setAttribute('data-multi-select-init', 'true');

      form.addEventListener('submit', () => {
        serializeMultiSelects(form);
      });

      form.addEventListener('htmx:beforeRequest', () => {
        serializeMultiSelects(form);
      });

      form.addEventListener('htmx:afterRequest', () => {
        restoreMultiSelects(form);
      });
    }
  );
}

function initRelationshipViews() {
  document.querySelectorAll('.relationship-field__view-link').forEach(
    /** @param {HTMLAnchorElement} link */ (link) => {
      const fieldName = link.getAttribute('data-view-for');
      const collection = link.getAttribute('data-collection');
      if (!fieldName || !collection) return;

      /** Update the view link visibility and href */
      const update = () => {
        // Look for hidden input by name (search widget) or select by id (legacy)
        const hidden = /** @type {HTMLInputElement | null} */ (
          document.querySelector(`input[type="hidden"][name="${fieldName.replace('field-', '')}"]`)
        );
        const select = /** @type {HTMLSelectElement | null} */ (document.getElementById(fieldName));
        const val = hidden ? hidden.value : (select ? select.value : '');
        if (val) {
          const href = '/admin/collections/' + collection + '/' + val;
          link.setAttribute('href', href);
          link.setAttribute('hx-get', href);
          link.style.display = '';
        } else {
          link.style.display = 'none';
        }
      };

      update();
      // Observe changes to the hidden input container
      const searchWidget = link.closest('.relationship-field')?.querySelector('.relationship-search__hidden');
      if (searchWidget) {
        new MutationObserver(update).observe(searchWidget, { childList: true, subtree: true, attributes: true });
      }
      // Also listen for select change (legacy/fallback)
      const select = document.getElementById(fieldName);
      if (select) select.addEventListener('change', update);
    }
  );
}

function init() {
  initRelationshipViews();
  initMultiSelectSerialization();
}

document.addEventListener('DOMContentLoaded', init);
document.addEventListener('htmx:afterSettle', init);
