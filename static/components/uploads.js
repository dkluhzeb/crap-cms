/**
 * Upload field preview behavior.
 *
 * Updates preview image and file info when the user selects a different
 * upload via the search widget or dropdown.
 *
 * Listens for `crap:change` events bubbling from the relationship-search
 * widget, reads data attributes from the hidden input, and updates the
 * `.upload-field__preview` and `.upload-field__info` elements.
 */

/**
 * Update preview and info elements.
 * @param {HTMLElement} wrapper - The [data-upload-field] wrapper
 * @param {string|null} thumbnailUrl
 * @param {string|null} filename
 * @param {boolean} isImage
 */
function updatePreview(wrapper, thumbnailUrl, filename, isImage) {
  const preview = wrapper.querySelector('.upload-field__preview');
  const info = wrapper.querySelector('.upload-field__info');

  if (preview) {
    if (thumbnailUrl && isImage) {
      preview.innerHTML = '<img src="' + thumbnailUrl + '" alt="Preview" />';
      preview.style.display = '';
    } else {
      preview.innerHTML = '';
      preview.style.display = 'none';
    }
  }

  if (info) {
    if (filename) {
      info.innerHTML =
        '<span class="material-symbols-outlined" style="font-size: 16px;">description</span>' +
        '<span class="upload-field__filename">' + filename + '</span>';
      info.style.display = '';
    } else {
      info.innerHTML = '';
      info.style.display = 'none';
    }
  }
}

function initUploadPreviews() {
  document.querySelectorAll('[data-upload-field]').forEach(
    /** @param {HTMLElement} wrapper */ (wrapper) => {
      if (wrapper.dataset.uploadInit) return;
      wrapper.dataset.uploadInit = '1';

      // Legacy: <select> for locale_locked fields
      const select = /** @type {HTMLSelectElement | null} */ (wrapper.querySelector('[data-upload-select]'));
      if (select) {
        select.addEventListener('change', () => {
          const option = select.options[select.selectedIndex];
          if (!option || !option.value) {
            updatePreview(wrapper, null, null, false);
            return;
          }
          updatePreview(
            wrapper,
            option.getAttribute('data-thumbnail'),
            option.getAttribute('data-filename'),
            option.getAttribute('data-is-image') === 'true',
          );
        });
        return;
      }

      // Search widget: listen for crap:change events (bubbles from relationship-search)
      wrapper.addEventListener('crap:change', () => {
        const hidden = wrapper.querySelector('.relationship-search__hidden input[type="hidden"]');
        if (!hidden || !hidden.value) {
          updatePreview(wrapper, null, null, false);
          return;
        }
        updatePreview(
          wrapper,
          hidden.getAttribute('data-thumbnail'),
          hidden.getAttribute('data-filename'),
          hidden.getAttribute('data-is-image') === 'true',
        );
      });
    }
  );
}

document.addEventListener('DOMContentLoaded', initUploadPreviews);
document.addEventListener('htmx:afterSettle', initUploadPreviews);
