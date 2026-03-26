/**
 * Focal point picker — `<crap-focal-point>`.
 *
 * Click the image to set the focal point — a crosshair marker shows
 * the position and hidden inputs are updated for form submission.
 *
 * @module focal-point
 */

class CrapFocalPoint extends HTMLElement {
  connectedCallback() {
    if (this._connected) return;
    this._connected = true;

    const img = this.querySelector('.focal-point img');
    const marker = this.querySelector('.focal-point__marker');
    const inputX = /** @type {HTMLInputElement|null} */ (this.querySelector('input[name="focal_x"]'));
    const inputY = /** @type {HTMLInputElement|null} */ (this.querySelector('input[name="focal_y"]'));
    if (!img || !marker || !inputX || !inputY) return;

    /** @param {number} x @param {number} y */
    const setMarker = (x, y) => {
      /** @type {HTMLElement} */ (marker).style.left = (x * 100) + '%';
      /** @type {HTMLElement} */ (marker).style.top = (y * 100) + '%';
      inputX.value = x.toFixed(4);
      inputY.value = y.toFixed(4);
    };

    // Position from existing data attributes (default center).
    // Use explicit NaN check — `|| 0.5` would treat a legitimate 0 as 0.5.
    const rawX = parseFloat(this.dataset.focalX);
    const rawY = parseFloat(this.dataset.focalY);
    setMarker(Number.isNaN(rawX) ? 0.5 : rawX, Number.isNaN(rawY) ? 0.5 : rawY);

    img.addEventListener('click', (e) => {
      const rect = img.getBoundingClientRect();
      const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
      const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
      setMarker(x, y);
    });
  }
}

customElements.define('crap-focal-point', CrapFocalPoint);
