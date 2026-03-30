// Populate the sidebar
//
// This is a script, and not included directly in the page, to control the total size of the book.
// The TOC contains an entry for each page, so if each page includes a copy of the TOC,
// the total size of the page becomes O(n**2).
class MDBookSidebarScrollbox extends HTMLElement {
    constructor() {
        super();
    }
    connectedCallback() {
        this.innerHTML = '<ol class="chapter"><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="introduction.html">Introduction</a></span></li><li class="chapter-item expanded "><li class="part-title">Getting Started</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/installation.html"><strong aria-hidden="true">1.</strong> Installation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/quickstart.html"><strong aria-hidden="true">2.</strong> Quick Start</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="getting-started/config-directory.html"><strong aria-hidden="true">3.</strong> Config Directory</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="configuration/crap-toml.html"><strong aria-hidden="true">4.</strong> crap.toml</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="locale/overview.html"><strong aria-hidden="true">5.</strong> Localization</a></span></li><li class="chapter-item expanded "><li class="part-title">CLI</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="cli/flags.html"><strong aria-hidden="true">6.</strong> Command-Line Reference</a></span></li><li class="chapter-item expanded "><li class="part-title">Collections</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="collections/overview.html"><strong aria-hidden="true">7.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="collections/definition-schema.html"><strong aria-hidden="true">8.</strong> Definition Schema</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="collections/versions.html"><strong aria-hidden="true">9.</strong> Versions &amp; Drafts</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="collections/soft-deletes.html"><strong aria-hidden="true">10.</strong> Soft Deletes</a></span></li><li class="chapter-item expanded "><li class="part-title">Fields</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/overview.html"><strong aria-hidden="true">11.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/text.html"><strong aria-hidden="true">12.</strong> Text</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/number.html"><strong aria-hidden="true">13.</strong> Number</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/textarea.html"><strong aria-hidden="true">14.</strong> Textarea</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/richtext.html"><strong aria-hidden="true">15.</strong> Rich Text</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/select.html"><strong aria-hidden="true">16.</strong> Select</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/radio.html"><strong aria-hidden="true">17.</strong> Radio</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/checkbox.html"><strong aria-hidden="true">18.</strong> Checkbox</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/date.html"><strong aria-hidden="true">19.</strong> Date</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/email.html"><strong aria-hidden="true">20.</strong> Email</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/json.html"><strong aria-hidden="true">21.</strong> JSON</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/relationship.html"><strong aria-hidden="true">22.</strong> Relationship</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/array.html"><strong aria-hidden="true">23.</strong> Array</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/group.html"><strong aria-hidden="true">24.</strong> Group</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/row.html"><strong aria-hidden="true">25.</strong> Row</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/collapsible.html"><strong aria-hidden="true">26.</strong> Collapsible</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/tabs.html"><strong aria-hidden="true">27.</strong> Tabs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/code.html"><strong aria-hidden="true">28.</strong> Code</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/join.html"><strong aria-hidden="true">29.</strong> Join</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/upload.html"><strong aria-hidden="true">30.</strong> Upload</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="fields/blocks.html"><strong aria-hidden="true">31.</strong> Blocks</a></span></li><li class="chapter-item expanded "><li class="part-title">Globals</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="globals/overview.html"><strong aria-hidden="true">32.</strong> Overview</a></span></li><li class="chapter-item expanded "><li class="part-title">Relationships</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="relationships/overview.html"><strong aria-hidden="true">33.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="relationships/delete-protection.html"><strong aria-hidden="true">34.</strong> Delete Protection</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="relationships/population-depth.html"><strong aria-hidden="true">35.</strong> Population Depth</a></span></li><li class="chapter-item expanded "><li class="part-title">Query &amp; Filters</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="query-and-filters/overview.html"><strong aria-hidden="true">36.</strong> Overview</a></span></li><li class="chapter-item expanded "><li class="part-title">Hooks</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/overview.html"><strong aria-hidden="true">37.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/lifecycle-events.html"><strong aria-hidden="true">38.</strong> Lifecycle Events</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/execution-order.html"><strong aria-hidden="true">39.</strong> Execution Order</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/field-hooks.html"><strong aria-hidden="true">40.</strong> Field Hooks</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/registered-hooks.html"><strong aria-hidden="true">41.</strong> Registered Hooks</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/hook-context.html"><strong aria-hidden="true">42.</strong> Hook Context</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="hooks/transaction-access.html"><strong aria-hidden="true">43.</strong> Transaction Access</a></span></li><li class="chapter-item expanded "><li class="part-title">Jobs &amp; Plugins</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="jobs/overview.html"><strong aria-hidden="true">44.</strong> Jobs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="plugins/overview.html"><strong aria-hidden="true">45.</strong> Plugins</a></span></li><li class="chapter-item expanded "><li class="part-title">Authentication &amp; Access Control</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="authentication/overview.html"><strong aria-hidden="true">46.</strong> Auth Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="authentication/auth-collections.html"><strong aria-hidden="true">47.</strong> Auth Collections</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="authentication/login-flow.html"><strong aria-hidden="true">48.</strong> Login Flow</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="authentication/custom-strategies.html"><strong aria-hidden="true">49.</strong> Custom Strategies</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="authentication/cli-user-creation.html"><strong aria-hidden="true">50.</strong> CLI User Creation</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="access-control/overview.html"><strong aria-hidden="true">51.</strong> Access Control Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="access-control/collection-level.html"><strong aria-hidden="true">52.</strong> Collection-Level Access</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="access-control/field-level.html"><strong aria-hidden="true">53.</strong> Field-Level Access</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="access-control/filter-constraints.html"><strong aria-hidden="true">54.</strong> Filter Constraints</a></span></li><li class="chapter-item expanded "><li class="part-title">Uploads &amp; Images</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="uploads/overview.html"><strong aria-hidden="true">55.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="uploads/image-processing.html"><strong aria-hidden="true">56.</strong> Image Processing</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="uploads/client-uploads.html"><strong aria-hidden="true">57.</strong> Client Uploads</a></span></li><li class="chapter-item expanded "><li class="part-title">Lua API</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/overview.html"><strong aria-hidden="true">58.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/collections.html"><strong aria-hidden="true">59.</strong> crap.collections</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/globals.html"><strong aria-hidden="true">60.</strong> crap.globals</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/hooks.html"><strong aria-hidden="true">61.</strong> crap.hooks</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/log.html"><strong aria-hidden="true">62.</strong> crap.log</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/json.html"><strong aria-hidden="true">63.</strong> crap.json</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/util.html"><strong aria-hidden="true">64.</strong> crap.util</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/auth.html"><strong aria-hidden="true">65.</strong> crap.auth</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/env.html"><strong aria-hidden="true">66.</strong> crap.env</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/http.html"><strong aria-hidden="true">67.</strong> crap.http</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/config.html"><strong aria-hidden="true">68.</strong> crap.config</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/email.html"><strong aria-hidden="true">69.</strong> crap.email</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/locale.html"><strong aria-hidden="true">70.</strong> crap.locale</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/crypto.html"><strong aria-hidden="true">71.</strong> crap.crypto</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/schema.html"><strong aria-hidden="true">72.</strong> crap.schema</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/richtext.html"><strong aria-hidden="true">73.</strong> crap.richtext</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/jobs.html"><strong aria-hidden="true">74.</strong> crap.jobs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="lua-api/filter-operators.html"><strong aria-hidden="true">75.</strong> Filter Operators</a></span></li><li class="chapter-item expanded "><li class="part-title">MCP (Model Context Protocol)</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="mcp/overview.html"><strong aria-hidden="true">76.</strong> Overview</a></span></li><li class="chapter-item expanded "><li class="part-title">gRPC API</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="grpc-api/overview.html"><strong aria-hidden="true">77.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="grpc-api/rpcs.html"><strong aria-hidden="true">78.</strong> RPCs</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="grpc-api/where-clause.html"><strong aria-hidden="true">79.</strong> Where Clause</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="grpc-api/authentication.html"><strong aria-hidden="true">80.</strong> Authentication</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="grpc-api/type-safety.html"><strong aria-hidden="true">81.</strong> Type Safety</a></span></li><li class="chapter-item expanded "><li class="part-title">Live Updates</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="live-updates/overview.html"><strong aria-hidden="true">82.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="live-updates/grpc-streaming.html"><strong aria-hidden="true">83.</strong> gRPC Streaming</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="live-updates/admin-sse.html"><strong aria-hidden="true">84.</strong> Admin SSE</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="live-updates/hooks.html"><strong aria-hidden="true">85.</strong> Hooks</a></span></li><li class="chapter-item expanded "><li class="part-title">Admin UI</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/overview.html"><strong aria-hidden="true">86.</strong> Overview</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/template-overlay.html"><strong aria-hidden="true">87.</strong> Template Overlay</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/template-context.html"><strong aria-hidden="true">88.</strong> Template Context</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/static-files.html"><strong aria-hidden="true">89.</strong> Static Files</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/display-conditions.html"><strong aria-hidden="true">90.</strong> Display Conditions</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="admin-ui/themes.html"><strong aria-hidden="true">91.</strong> Themes</a></span></li><li class="chapter-item expanded "><li class="part-title">Internals</li></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="database/overview.html"><strong aria-hidden="true">92.</strong> Database</a></span></li><li class="chapter-item expanded "><span class="chapter-link-wrapper"><a href="internals/api-surface-comparison.html"><strong aria-hidden="true">93.</strong> API Surface Comparison</a></span></li></ol>';
        // Set the current, active page, and reveal it if it's hidden
        let current_page = document.location.href.toString().split('#')[0].split('?')[0];
        if (current_page.endsWith('/')) {
            current_page += 'index.html';
        }
        const links = Array.prototype.slice.call(this.querySelectorAll('a'));
        const l = links.length;
        for (let i = 0; i < l; ++i) {
            const link = links[i];
            const href = link.getAttribute('href');
            if (href && !href.startsWith('#') && !/^(?:[a-z+]+:)?\/\//.test(href)) {
                link.href = path_to_root + href;
            }
            // The 'index' page is supposed to alias the first chapter in the book.
            if (link.href === current_page
                || i === 0
                && path_to_root === ''
                && current_page.endsWith('/index.html')) {
                link.classList.add('active');
                let parent = link.parentElement;
                while (parent) {
                    if (parent.tagName === 'LI' && parent.classList.contains('chapter-item')) {
                        parent.classList.add('expanded');
                    }
                    parent = parent.parentElement;
                }
            }
        }
        // Track and set sidebar scroll position
        this.addEventListener('click', e => {
            if (e.target.tagName === 'A') {
                const clientRect = e.target.getBoundingClientRect();
                const sidebarRect = this.getBoundingClientRect();
                sessionStorage.setItem('sidebar-scroll-offset', clientRect.top - sidebarRect.top);
            }
        }, { passive: true });
        const sidebarScrollOffset = sessionStorage.getItem('sidebar-scroll-offset');
        sessionStorage.removeItem('sidebar-scroll-offset');
        if (sidebarScrollOffset !== null) {
            // preserve sidebar scroll position when navigating via links within sidebar
            const activeSection = this.querySelector('.active');
            if (activeSection) {
                const clientRect = activeSection.getBoundingClientRect();
                const sidebarRect = this.getBoundingClientRect();
                const currentOffset = clientRect.top - sidebarRect.top;
                this.scrollTop += currentOffset - parseFloat(sidebarScrollOffset);
            }
        } else {
            // scroll sidebar to current active section when navigating via
            // 'next/previous chapter' buttons
            const activeSection = document.querySelector('#mdbook-sidebar .active');
            if (activeSection) {
                activeSection.scrollIntoView({ block: 'center' });
            }
        }
        // Toggle buttons
        const sidebarAnchorToggles = document.querySelectorAll('.chapter-fold-toggle');
        function toggleSection(ev) {
            ev.currentTarget.parentElement.parentElement.classList.toggle('expanded');
        }
        Array.from(sidebarAnchorToggles).forEach(el => {
            el.addEventListener('click', toggleSection);
        });
    }
}
window.customElements.define('mdbook-sidebar-scrollbox', MDBookSidebarScrollbox);


// ---------------------------------------------------------------------------
// Support for dynamically adding headers to the sidebar.

(function() {
    // This is used to detect which direction the page has scrolled since the
    // last scroll event.
    let lastKnownScrollPosition = 0;
    // This is the threshold in px from the top of the screen where it will
    // consider a header the "current" header when scrolling down.
    const defaultDownThreshold = 150;
    // Same as defaultDownThreshold, except when scrolling up.
    const defaultUpThreshold = 300;
    // The threshold is a virtual horizontal line on the screen where it
    // considers the "current" header to be above the line. The threshold is
    // modified dynamically to handle headers that are near the bottom of the
    // screen, and to slightly offset the behavior when scrolling up vs down.
    let threshold = defaultDownThreshold;
    // This is used to disable updates while scrolling. This is needed when
    // clicking the header in the sidebar, which triggers a scroll event. It
    // is somewhat finicky to detect when the scroll has finished, so this
    // uses a relatively dumb system of disabling scroll updates for a short
    // time after the click.
    let disableScroll = false;
    // Array of header elements on the page.
    let headers;
    // Array of li elements that are initially collapsed headers in the sidebar.
    // I'm not sure why eslint seems to have a false positive here.
    // eslint-disable-next-line prefer-const
    let headerToggles = [];
    // This is a debugging tool for the threshold which you can enable in the console.
    let thresholdDebug = false;

    // Updates the threshold based on the scroll position.
    function updateThreshold() {
        const scrollTop = window.pageYOffset || document.documentElement.scrollTop;
        const windowHeight = window.innerHeight;
        const documentHeight = document.documentElement.scrollHeight;

        // The number of pixels below the viewport, at most documentHeight.
        // This is used to push the threshold down to the bottom of the page
        // as the user scrolls towards the bottom.
        const pixelsBelow = Math.max(0, documentHeight - (scrollTop + windowHeight));
        // The number of pixels above the viewport, at least defaultDownThreshold.
        // Similar to pixelsBelow, this is used to push the threshold back towards
        // the top when reaching the top of the page.
        const pixelsAbove = Math.max(0, defaultDownThreshold - scrollTop);
        // How much the threshold should be offset once it gets close to the
        // bottom of the page.
        const bottomAdd = Math.max(0, windowHeight - pixelsBelow - defaultDownThreshold);
        let adjustedBottomAdd = bottomAdd;

        // Adjusts bottomAdd for a small document. The calculation above
        // assumes the document is at least twice the windowheight in size. If
        // it is less than that, then bottomAdd needs to be shrunk
        // proportional to the difference in size.
        if (documentHeight < windowHeight * 2) {
            const maxPixelsBelow = documentHeight - windowHeight;
            const t = 1 - pixelsBelow / Math.max(1, maxPixelsBelow);
            const clamp = Math.max(0, Math.min(1, t));
            adjustedBottomAdd *= clamp;
        }

        let scrollingDown = true;
        if (scrollTop < lastKnownScrollPosition) {
            scrollingDown = false;
        }

        if (scrollingDown) {
            // When scrolling down, move the threshold up towards the default
            // downwards threshold position. If near the bottom of the page,
            // adjustedBottomAdd will offset the threshold towards the bottom
            // of the page.
            const amountScrolledDown = scrollTop - lastKnownScrollPosition;
            const adjustedDefault = defaultDownThreshold + adjustedBottomAdd;
            threshold = Math.max(adjustedDefault, threshold - amountScrolledDown);
        } else {
            // When scrolling up, move the threshold down towards the default
            // upwards threshold position. If near the bottom of the page,
            // quickly transition the threshold back up where it normally
            // belongs.
            const amountScrolledUp = lastKnownScrollPosition - scrollTop;
            const adjustedDefault = defaultUpThreshold - pixelsAbove
                + Math.max(0, adjustedBottomAdd - defaultDownThreshold);
            threshold = Math.min(adjustedDefault, threshold + amountScrolledUp);
        }

        if (documentHeight <= windowHeight) {
            threshold = 0;
        }

        if (thresholdDebug) {
            const id = 'mdbook-threshold-debug-data';
            let data = document.getElementById(id);
            if (data === null) {
                data = document.createElement('div');
                data.id = id;
                data.style.cssText = `
                    position: fixed;
                    top: 50px;
                    right: 10px;
                    background-color: 0xeeeeee;
                    z-index: 9999;
                    pointer-events: none;
                `;
                document.body.appendChild(data);
            }
            data.innerHTML = `
                <table>
                  <tr><td>documentHeight</td><td>${documentHeight.toFixed(1)}</td></tr>
                  <tr><td>windowHeight</td><td>${windowHeight.toFixed(1)}</td></tr>
                  <tr><td>scrollTop</td><td>${scrollTop.toFixed(1)}</td></tr>
                  <tr><td>pixelsAbove</td><td>${pixelsAbove.toFixed(1)}</td></tr>
                  <tr><td>pixelsBelow</td><td>${pixelsBelow.toFixed(1)}</td></tr>
                  <tr><td>bottomAdd</td><td>${bottomAdd.toFixed(1)}</td></tr>
                  <tr><td>adjustedBottomAdd</td><td>${adjustedBottomAdd.toFixed(1)}</td></tr>
                  <tr><td>scrollingDown</td><td>${scrollingDown}</td></tr>
                  <tr><td>threshold</td><td>${threshold.toFixed(1)}</td></tr>
                </table>
            `;
            drawDebugLine();
        }

        lastKnownScrollPosition = scrollTop;
    }

    function drawDebugLine() {
        if (!document.body) {
            return;
        }
        const id = 'mdbook-threshold-debug-line';
        const existingLine = document.getElementById(id);
        if (existingLine) {
            existingLine.remove();
        }
        const line = document.createElement('div');
        line.id = id;
        line.style.cssText = `
            position: fixed;
            top: ${threshold}px;
            left: 0;
            width: 100vw;
            height: 2px;
            background-color: red;
            z-index: 9999;
            pointer-events: none;
        `;
        document.body.appendChild(line);
    }

    function mdbookEnableThresholdDebug() {
        thresholdDebug = true;
        updateThreshold();
        drawDebugLine();
    }

    window.mdbookEnableThresholdDebug = mdbookEnableThresholdDebug;

    // Updates which headers in the sidebar should be expanded. If the current
    // header is inside a collapsed group, then it, and all its parents should
    // be expanded.
    function updateHeaderExpanded(currentA) {
        // Add expanded to all header-item li ancestors.
        let current = currentA.parentElement;
        while (current) {
            if (current.tagName === 'LI' && current.classList.contains('header-item')) {
                current.classList.add('expanded');
            }
            current = current.parentElement;
        }
    }

    // Updates which header is marked as the "current" header in the sidebar.
    // This is done with a virtual Y threshold, where headers at or below
    // that line will be considered the current one.
    function updateCurrentHeader() {
        if (!headers || !headers.length) {
            return;
        }

        // Reset the classes, which will be rebuilt below.
        const els = document.getElementsByClassName('current-header');
        for (const el of els) {
            el.classList.remove('current-header');
        }
        for (const toggle of headerToggles) {
            toggle.classList.remove('expanded');
        }

        // Find the last header that is above the threshold.
        let lastHeader = null;
        for (const header of headers) {
            const rect = header.getBoundingClientRect();
            if (rect.top <= threshold) {
                lastHeader = header;
            } else {
                break;
            }
        }
        if (lastHeader === null) {
            lastHeader = headers[0];
            const rect = lastHeader.getBoundingClientRect();
            const windowHeight = window.innerHeight;
            if (rect.top >= windowHeight) {
                return;
            }
        }

        // Get the anchor in the summary.
        const href = '#' + lastHeader.id;
        const a = [...document.querySelectorAll('.header-in-summary')]
            .find(element => element.getAttribute('href') === href);
        if (!a) {
            return;
        }

        a.classList.add('current-header');

        updateHeaderExpanded(a);
    }

    // Updates which header is "current" based on the threshold line.
    function reloadCurrentHeader() {
        if (disableScroll) {
            return;
        }
        updateThreshold();
        updateCurrentHeader();
    }


    // When clicking on a header in the sidebar, this adjusts the threshold so
    // that it is located next to the header. This is so that header becomes
    // "current".
    function headerThresholdClick(event) {
        // See disableScroll description why this is done.
        disableScroll = true;
        setTimeout(() => {
            disableScroll = false;
        }, 100);
        // requestAnimationFrame is used to delay the update of the "current"
        // header until after the scroll is done, and the header is in the new
        // position.
        requestAnimationFrame(() => {
            requestAnimationFrame(() => {
                // Closest is needed because if it has child elements like <code>.
                const a = event.target.closest('a');
                const href = a.getAttribute('href');
                const targetId = href.substring(1);
                const targetElement = document.getElementById(targetId);
                if (targetElement) {
                    threshold = targetElement.getBoundingClientRect().bottom;
                    updateCurrentHeader();
                }
            });
        });
    }

    // Takes the nodes from the given head and copies them over to the
    // destination, along with some filtering.
    function filterHeader(source, dest) {
        const clone = source.cloneNode(true);
        clone.querySelectorAll('mark').forEach(mark => {
            mark.replaceWith(...mark.childNodes);
        });
        dest.append(...clone.childNodes);
    }

    // Scans page for headers and adds them to the sidebar.
    document.addEventListener('DOMContentLoaded', function() {
        const activeSection = document.querySelector('#mdbook-sidebar .active');
        if (activeSection === null) {
            return;
        }

        const main = document.getElementsByTagName('main')[0];
        headers = Array.from(main.querySelectorAll('h2, h3, h4, h5, h6'))
            .filter(h => h.id !== '' && h.children.length && h.children[0].tagName === 'A');

        if (headers.length === 0) {
            return;
        }

        // Build a tree of headers in the sidebar.

        const stack = [];

        const firstLevel = parseInt(headers[0].tagName.charAt(1));
        for (let i = 1; i < firstLevel; i++) {
            const ol = document.createElement('ol');
            ol.classList.add('section');
            if (stack.length > 0) {
                stack[stack.length - 1].ol.appendChild(ol);
            }
            stack.push({level: i + 1, ol: ol});
        }

        // The level where it will start folding deeply nested headers.
        const foldLevel = 3;

        for (let i = 0; i < headers.length; i++) {
            const header = headers[i];
            const level = parseInt(header.tagName.charAt(1));

            const currentLevel = stack[stack.length - 1].level;
            if (level > currentLevel) {
                // Begin nesting to this level.
                for (let nextLevel = currentLevel + 1; nextLevel <= level; nextLevel++) {
                    const ol = document.createElement('ol');
                    ol.classList.add('section');
                    const last = stack[stack.length - 1];
                    const lastChild = last.ol.lastChild;
                    // Handle the case where jumping more than one nesting
                    // level, which doesn't have a list item to place this new
                    // list inside of.
                    if (lastChild) {
                        lastChild.appendChild(ol);
                    } else {
                        last.ol.appendChild(ol);
                    }
                    stack.push({level: nextLevel, ol: ol});
                }
            } else if (level < currentLevel) {
                while (stack.length > 1 && stack[stack.length - 1].level > level) {
                    stack.pop();
                }
            }

            const li = document.createElement('li');
            li.classList.add('header-item');
            li.classList.add('expanded');
            if (level < foldLevel) {
                li.classList.add('expanded');
            }
            const span = document.createElement('span');
            span.classList.add('chapter-link-wrapper');
            const a = document.createElement('a');
            span.appendChild(a);
            a.href = '#' + header.id;
            a.classList.add('header-in-summary');
            filterHeader(header.children[0], a);
            a.addEventListener('click', headerThresholdClick);
            const nextHeader = headers[i + 1];
            if (nextHeader !== undefined) {
                const nextLevel = parseInt(nextHeader.tagName.charAt(1));
                if (nextLevel > level && level >= foldLevel) {
                    const toggle = document.createElement('a');
                    toggle.classList.add('chapter-fold-toggle');
                    toggle.classList.add('header-toggle');
                    toggle.addEventListener('click', () => {
                        li.classList.toggle('expanded');
                    });
                    const toggleDiv = document.createElement('div');
                    toggleDiv.textContent = '❱';
                    toggle.appendChild(toggleDiv);
                    span.appendChild(toggle);
                    headerToggles.push(li);
                }
            }
            li.appendChild(span);

            const currentParent = stack[stack.length - 1];
            currentParent.ol.appendChild(li);
        }

        const onThisPage = document.createElement('div');
        onThisPage.classList.add('on-this-page');
        onThisPage.append(stack[0].ol);
        const activeItemSpan = activeSection.parentElement;
        activeItemSpan.after(onThisPage);
    });

    document.addEventListener('DOMContentLoaded', reloadCurrentHeader);
    document.addEventListener('scroll', reloadCurrentHeader, { passive: true });
})();

