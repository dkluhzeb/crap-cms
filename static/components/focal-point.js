/**
 * Focal point picker behavior.
 *
 * Attaches to `[data-focal-point]` wrappers on the upload collection edit
 * page. Click the image to set the focal point — a crosshair marker shows
 * the position and hidden inputs are updated for form submission.
 *
 * @module focal-point
 */

import { registerInit } from './actions.js';

/**
 * Initialize focal point pickers.
 * Finds all `[data-focal-point]` wrappers and attaches click behavior.
 */
function initFocalPoints() {
  const wrappers = document.querySelectorAll('[data-focal-point]');

  for (const wrapper of wrappers) {
    if (wrapper.dataset.focalPointBound) continue;
    wrapper.dataset.focalPointBound = '1';

    const img = wrapper.querySelector('.focal-point img');
    const marker = wrapper.querySelector('.focal-point__marker');
    const inputX = wrapper.querySelector('input[name="focal_x"]');
    const inputY = wrapper.querySelector('input[name="focal_y"]');
    if (!img || !marker || !inputX || !inputY) continue;

    /** @param {number} x @param {number} y */
    function setMarker(x, y) {
      marker.style.left = (x * 100) + '%';
      marker.style.top = (y * 100) + '%';
      inputX.value = x.toFixed(4);
      inputY.value = y.toFixed(4);
    }

    // Position from existing data attributes (default center)
    const initX = parseFloat(wrapper.dataset.focalX) || 0.5;
    const initY = parseFloat(wrapper.dataset.focalY) || 0.5;
    setMarker(initX, initY);

    // Click handler
    img.addEventListener('click', (e) => {
      const rect = img.getBoundingClientRect();
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
      setMarker(x, y);
    });
  }
}

registerInit(initFocalPoints);
