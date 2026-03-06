/**
 * Mobile sidebar toggle behavior.
 *
 * Uses the actions.js delegation system — the hamburger button and
 * backdrop both use `data-action="toggle-sidebar"`.
 *
 * @module sidebar-toggle
 */

import { registerAction } from './actions.js';

function closeSidebar() {
  document.querySelector('.sidebar')?.classList.remove('sidebar--open');
  document.querySelector('.sidebar-backdrop')?.classList.remove('sidebar-backdrop--visible');
}

function toggleSidebar() {
  const sidebar = document.querySelector('.sidebar');
  const backdrop = document.querySelector('.sidebar-backdrop');
  if (!sidebar) return;
  const opening = !sidebar.classList.contains('sidebar--open');
  sidebar.classList.toggle('sidebar--open', opening);
  backdrop?.classList.toggle('sidebar-backdrop--visible', opening);
}

registerAction('toggle-sidebar', toggleSidebar);

// Close on Escape
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeSidebar();
});

// Close on navigation (HTMX page transitions)
document.addEventListener('htmx:beforeRequest', closeSidebar);
