local M = {}

function M.up()
	crap.log.info("Seeding Crap Studio data...")
	local opts = { overrideAccess = true }

	-- ========================
	-- USERS (6)
	-- ========================
	local admin = crap.collections.create("users", {
		email = "admin@crap.studio",
		password = "admin123",
		name = "Alex Morgan",
		role = "admin",
		skills = { "strategy", "design" },
		bio = "Founder & Creative Director at Crap Studio. 15 years of experience in digital design and brand strategy.",
	}, opts)

	local director = crap.collections.create("users", {
		email = "sam@crap.studio",
		password = "password123",
		name = "Sam Chen",
		role = "director",
		skills = { "development", "strategy" },
		bio = "Technical Director. Full-stack architect with a passion for performance and clean code.",
	}, opts)

	local editor = crap.collections.create("users", {
		email = "jordan@crap.studio",
		password = "password123",
		name = "Jordan Rivera",
		role = "editor",
		skills = { "copywriting", "strategy" },
		bio = "Content Lead. Storyteller, strategist, and occasional poet.",
	}, opts)

	local designer = crap.collections.create("users", {
		email = "taylor@crap.studio",
		password = "password123",
		name = "Taylor Kim",
		role = "author",
		skills = { "design", "motion", "3d" },
		bio = "Senior Designer. Specializes in motion graphics and 3D visualization.",
	}, opts)

	local dev = crap.collections.create("users", {
		email = "casey@crap.studio",
		password = "password123",
		name = "Casey Brooks",
		role = "author",
		skills = { "development", "3d" },
		bio = "Senior Developer. WebGL enthusiast and creative technologist.",
	}, opts)

	local photographer = crap.collections.create("users", {
		email = "riley@crap.studio",
		password = "password123",
		name = "Riley Patel",
		role = "author",
		skills = { "photography", "design" },
		bio = "Visual Artist. Commercial and editorial photographer.",
	}, opts)

	-- ========================
	-- CATEGORIES (6)
	-- ========================
	local cat_design = crap.collections.create("categories", {
		title = "Design",
		slug = "design",
		description = "Visual design, branding, and UI/UX",
		color = "#8b5cf6",
	}, opts)

	local cat_dev = crap.collections.create("categories", {
		title = "Development",
		slug = "development",
		description = "Web development, engineering, and architecture",
		color = "#3b82f6",
	}, opts)

	local cat_strategy = crap.collections.create("categories", {
		title = "Strategy",
		slug = "strategy",
		description = "Digital strategy and consulting",
		color = "#10b981",
	}, opts)

	local cat_motion = crap.collections.create("categories", {
		title = "Motion",
		slug = "motion",
		description = "Motion graphics and animation",
		color = "#f59e0b",
	}, opts)

	local cat_brand = crap.collections.create("categories", {
		title = "Branding",
		slug = "branding",
		description = "Brand identity and visual systems",
		color = "#ec4899",
	}, opts)

	local cat_culture = crap.collections.create("categories", {
		title = "Culture",
		slug = "culture",
		description = "Studio life, team updates, and events",
		color = "#6366f1",
	}, opts)

	-- ========================
	-- TAGS (12)
	-- ========================
	local tag_react = crap.collections.create("tags", { name = "React", slug = "react", tag_type = "technology" }, opts)
	local tag_rust = crap.collections.create("tags", { name = "Rust", slug = "rust", tag_type = "technology" }, opts)
	local tag_webgl = crap.collections.create("tags", { name = "WebGL", slug = "webgl", tag_type = "technology" }, opts)
	local tag_figma = crap.collections.create("tags", { name = "Figma", slug = "figma", tag_type = "technology" }, opts)
	local tag_ux = crap.collections.create("tags", { name = "UX Research", slug = "ux-research", tag_type = "topic" }, opts)
	local tag_a11y = crap.collections.create("tags", { name = "Accessibility", slug = "accessibility", tag_type = "topic" }, opts)
	local tag_perf = crap.collections.create("tags", { name = "Performance", slug = "performance", tag_type = "topic" }, opts)
	local tag_ds = crap.collections.create("tags", { name = "Design Systems", slug = "design-systems", tag_type = "topic" }, opts)
	local tag_ai = crap.collections.create("tags", { name = "AI/ML", slug = "ai-ml", tag_type = "technology" }, opts)
	local tag_fintech = crap.collections.create("tags", { name = "Fintech", slug = "fintech", tag_type = "industry" }, opts)
	local tag_health = crap.collections.create("tags", { name = "Healthcare", slug = "healthcare", tag_type = "industry" }, opts)
	local tag_ecom = crap.collections.create("tags", { name = "E-commerce", slug = "e-commerce", tag_type = "industry" }, opts)

	-- ========================
	-- CLIENTS (5)
	-- ========================
	local client_nova = crap.collections.create("clients", {
		company_name = "Nova Financial",
		website = "https://novafinancial.com",
		since = "2023-03",
		contact_name = "Michael Torres",
		contact_email = "michael@novafinancial.com",
		industry = "finance",
		notes = "Enterprise client. Annual retainer for ongoing design work.",
	}, opts)

	local client_pulse = crap.collections.create("clients", {
		company_name = "Pulse Health",
		website = "https://pulsehealth.io",
		since = "2024-01",
		contact_name = "Sarah Kim",
		contact_email = "sarah@pulsehealth.io",
		industry = "healthcare",
		notes = "Series B startup. Building their patient portal.",
	}, opts)

	local client_apex = crap.collections.create("clients", {
		company_name = "Apex Retail",
		website = "https://apexretail.com",
		since = "2023-09",
		contact_name = "David Okonkwo",
		contact_email = "david@apexretail.com",
		industry = "retail",
	}, opts)

	local client_verde = crap.collections.create("clients", {
		company_name = "Verde Education",
		website = "https://verde.edu",
		since = "2024-06",
		contact_name = "Lisa Chang",
		contact_email = "lisa@verde.edu",
		industry = "education",
	}, opts)

	local client_echo = crap.collections.create("clients", {
		company_name = "Echo Media Group",
		website = "https://echomedia.co",
		since = "2025-01",
		contact_name = "James Wright",
		contact_email = "james@echomedia.co",
		industry = "media",
	}, opts)

	-- ========================
	-- SERVICES (5)
	-- ========================
	local svc_brand = crap.collections.create("services", {
		title = "Brand Identity",
		slug = "brand-identity",
		description = "Complete brand identity systems including logo design, typography, color palette, and brand guidelines.",
		active = true,
		sort_order = 1,
		pricing_type = "fixed",
		price_range = { min_price = 15000, max_price = 50000, currency = "USD" },
		features = {
			{ title = "Logo & mark design", included = true },
			{ title = "Typography system", included = true },
			{ title = "Color palette", included = true },
			{ title = "Brand guidelines PDF", included = true },
			{ title = "Social media templates", included = true },
		},
		icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/></svg>',
	}, opts)

	local svc_web = crap.collections.create("services", {
		title = "Web Development",
		slug = "web-development",
		description = "Custom web applications built with modern technologies. From marketing sites to complex web apps.",
		active = true,
		sort_order = 2,
		pricing_type = "custom",
		features = {
			{ title = "Custom architecture", included = true },
			{ title = "Responsive design", included = true },
			{ title = "CMS integration", included = true },
			{ title = "Performance optimization", included = true },
			{ title = "Accessibility audit", included = true },
			{ title = "Ongoing maintenance", included = false },
		},
		icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>',
	}, opts)

	local svc_ux = crap.collections.create("services", {
		title = "UX Design",
		slug = "ux-design",
		description = "User experience design and research. We design interfaces that people love to use.",
		active = true,
		sort_order = 3,
		pricing_type = "hourly",
		price_range = { min_price = 150, max_price = 250, currency = "USD" },
		features = {
			{ title = "User research", included = true },
			{ title = "Wireframes & prototypes", included = true },
			{ title = "Usability testing", included = true },
			{ title = "Design system", included = true },
		},
		icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2" ry="2"/><line x1="3" y1="9" x2="21" y2="9"/><line x1="9" y1="21" x2="9" y2="9"/></svg>',
	}, opts)

	local svc_motion = crap.collections.create("services", {
		title = "Motion Design",
		slug = "motion-design",
		description = "Animation and motion graphics for web, social media, and presentations.",
		active = true,
		sort_order = 4,
		pricing_type = "fixed",
		price_range = { min_price = 5000, max_price = 25000, currency = "USD" },
		features = {
			{ title = "Animated logos", included = true },
			{ title = "Product animations", included = true },
			{ title = "Social media content", included = true },
			{ title = "Presentation decks", included = true },
		},
		icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3"/></svg>',
	}, opts)

	local svc_consult = crap.collections.create("services", {
		title = "Digital Strategy",
		slug = "digital-strategy",
		description = "Strategic consulting for digital transformation. We help you define your digital roadmap.",
		active = true,
		sort_order = 5,
		pricing_type = "hourly",
		price_range = { min_price = 200, max_price = 350, currency = "USD" },
		features = {
			{ title = "Competitive analysis", included = true },
			{ title = "Technology assessment", included = true },
			{ title = "Roadmap planning", included = true },
			{ title = "Implementation support", included = false },
		},
		icon = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>',
	}, opts)

	-- ========================
	-- PROJECTS (10)
	-- ========================
	local proj_nova_rebrand = crap.collections.create("projects", {
		title = "Nova Financial Rebrand",
		slug = "nova-financial-rebrand",
		excerpt = "Complete brand overhaul for a leading fintech company, transitioning from legacy banking aesthetics to a modern, approachable identity.",
		status = "completed",
		priority = "high",
		featured = true,
		start_date = "2024-03-01",
		end_date = "2024-08-15",
		client = client_nova.id,
		team = { admin.id, designer.id, photographer.id },
		categories = { cat_brand.id, cat_design.id },
		tags = { tag_fintech.id, tag_figma.id, tag_ds.id },
		budget = 85000,
		deliverables = {
			{ title = "Brand guidelines", completed = true },
			{ title = "Logo suite", completed = true },
			{ title = "Marketing collateral", completed = true },
			{ title = "Digital asset library", completed = true },
		},
		content = {
			{
				_block_type = "richtext",
				body = "<h2>The Challenge</h2><p>Nova Financial needed to shed its traditional banking image and appeal to a younger demographic while maintaining trust with existing customers.</p><h2>Our Approach</h2><p>We conducted extensive user research and competitive analysis before developing a brand system that balances modernity with financial credibility.</p>",
			},
			{
				_block_type = "stats",
				items = {
					{ value = "340%", label = "Brand recognition increase" },
					{ value = "28%", label = "New account signups" },
					{ value = "92%", label = "Customer approval rating" },
				},
			},
		},
		published_at = "2024-09-01T10:00:00Z",
	}, opts)

	local proj_pulse_portal = crap.collections.create("projects", {
		title = "Pulse Health Patient Portal",
		slug = "pulse-health-portal",
		excerpt = "HIPAA-compliant patient portal with real-time health monitoring, appointment scheduling, and secure messaging.",
		status = "in_progress",
		priority = "urgent",
		featured = true,
		start_date = "2024-09-01",
		client = client_pulse.id,
		team = { director.id, dev.id, designer.id },
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_react.id, tag_rust.id, tag_a11y.id, tag_health.id },
		budget = 180000,
		deliverables = {
			{ title = "Patient dashboard", completed = true },
			{ title = "Appointment system", completed = true },
			{ title = "Messaging module", completed = false },
			{ title = "Health data visualization", completed = false },
		},
		content = {
			{
				_block_type = "richtext",
				body = "<h2>Building for Healthcare</h2><p>The Pulse Health portal is designed to make healthcare accessible and intuitive. Every interaction is crafted with WCAG 2.1 AA compliance and HIPAA security requirements in mind.</p>",
			},
		},
	}, opts)

	local proj_apex_ecom = crap.collections.create("projects", {
		title = "Apex Retail E-Commerce Platform",
		slug = "apex-retail-ecommerce",
		excerpt = "High-performance e-commerce platform handling 50K+ concurrent users with sub-second page loads.",
		status = "completed",
		priority = "high",
		featured = false,
		start_date = "2023-11-01",
		end_date = "2024-06-30",
		client = client_apex.id,
		team = { director.id, dev.id },
		categories = { cat_dev.id },
		tags = { tag_react.id, tag_perf.id, tag_ecom.id },
		budget = 120000,
		deliverables = {
			{ title = "Product catalog", completed = true },
			{ title = "Checkout flow", completed = true },
			{ title = "Inventory management", completed = true },
			{ title = "Analytics dashboard", completed = true },
		},
		content = {
			{
				_block_type = "richtext",
				body = "<h2>Performance at Scale</h2><p>Built on a modern stack with edge caching and optimistic UI updates, the Apex platform delivers a blazing-fast shopping experience even under peak load.</p>",
			},
			{
				_block_type = "stats",
				items = {
					{ value = "0.8s", label = "Average page load" },
					{ value = "50K+", label = "Concurrent users" },
					{ value = "99.9%", label = "Uptime" },
				},
			},
		},
		published_at = "2024-07-15T10:00:00Z",
	}, opts)

	local proj_verde_lms = crap.collections.create("projects", {
		title = "Verde Learning Management System",
		slug = "verde-lms",
		excerpt = "Interactive learning platform with real-time collaboration, video streaming, and adaptive assessments.",
		status = "in_progress",
		priority = "normal",
		featured = false,
		start_date = "2024-11-01",
		client = client_verde.id,
		team = { dev.id, designer.id },
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_react.id, tag_webgl.id, tag_a11y.id },
		budget = 95000,
		deliverables = {
			{ title = "Course builder", completed = true },
			{ title = "Student dashboard", completed = false },
			{ title = "Assessment engine", completed = false },
		},
	}, opts)

	local proj_echo_cms = crap.collections.create("projects", {
		title = "Echo Media Content Hub",
		slug = "echo-media-content-hub",
		excerpt = "Multi-tenant content management platform for Echo Media's portfolio of digital publications.",
		status = "planning",
		priority = "normal",
		start_date = "2026-02-01",
		client = client_echo.id,
		team = { director.id, dev.id },
		categories = { cat_dev.id, cat_strategy.id },
		tags = { tag_rust.id, tag_ai.id },
		budget = 150000,
	}, opts)

	local proj_ds = crap.collections.create("projects", {
		title = "Crap Design System",
		slug = "crap-design-system",
		excerpt = "Internal design system powering all Crap Studio projects. Components, tokens, and documentation.",
		status = "in_progress",
		priority = "high",
		featured = true,
		start_date = "2024-01-01",
		team = { admin.id, designer.id, dev.id },
		categories = { cat_design.id, cat_dev.id },
		tags = { tag_figma.id, tag_ds.id, tag_react.id },
		deliverables = {
			{ title = "Core tokens", completed = true },
			{ title = "Component library", completed = true },
			{ title = "Documentation site", completed = false },
			{ title = "Figma plugin", completed = false },
		},
		content = {
			{
				_block_type = "richtext",
				body = "<h2>One System, Every Project</h2><p>The Crap Design System ensures consistency across all our deliverables while giving each project room to breathe with customizable tokens and composable components.</p>",
			},
		},
	}, opts)

	local proj_brand_motion = crap.collections.create("projects", {
		title = "Nova Animated Brand Assets",
		slug = "nova-brand-motion",
		excerpt = "Animated logo, social media templates, and presentation toolkit for Nova Financial's rebrand.",
		status = "completed",
		priority = "normal",
		start_date = "2024-08-01",
		end_date = "2024-10-15",
		client = client_nova.id,
		team = { designer.id },
		categories = { cat_motion.id, cat_brand.id },
		tags = { tag_figma.id },
		budget = 25000,
		deliverables = {
			{ title = "Animated logo", completed = true },
			{ title = "Social templates (10)", completed = true },
			{ title = "Pitch deck template", completed = true },
		},
		published_at = "2024-11-01T10:00:00Z",
	}, opts)

	local proj_pulse_brand = crap.collections.create("projects", {
		title = "Pulse Health Brand Refresh",
		slug = "pulse-health-brand",
		excerpt = "Brand refresh to align with Pulse Health's expanded product offering and Series B positioning.",
		status = "review",
		priority = "normal",
		start_date = "2025-06-01",
		end_date = "2025-09-30",
		client = client_pulse.id,
		team = { admin.id, photographer.id },
		categories = { cat_brand.id },
		tags = { tag_health.id, tag_figma.id },
		budget = 45000,
	}, opts)

	local proj_apex_mobile = crap.collections.create("projects", {
		title = "Apex Mobile Shopping App",
		slug = "apex-mobile-app",
		excerpt = "Native mobile shopping experience with AR try-on, personalized recommendations, and one-tap checkout.",
		status = "planning",
		priority = "high",
		start_date = "2026-04-01",
		client = client_apex.id,
		team = { director.id, dev.id, designer.id },
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_react.id, tag_ecom.id, tag_ai.id },
		budget = 200000,
	}, opts)

	local proj_internal_site = crap.collections.create("projects", {
		title = "Crap Studio Website",
		slug = "crap-website",
		excerpt = "Our own website, built with Crap CMS. Dogfooding at its finest.",
		status = "in_progress",
		priority = "normal",
		featured = false,
		start_date = "2025-01-01",
		team = { admin.id, director.id, editor.id },
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_rust.id, tag_perf.id },
		deliverables = {
			{ title = "Homepage", completed = true },
			{ title = "Portfolio section", completed = true },
			{ title = "Blog", completed = false },
			{ title = "Contact form", completed = false },
		},
	}, opts)

	-- ========================
	-- POSTS (20)
	-- ========================
	local function create_post(data)
		return crap.collections.create("posts", data, opts)
	end

	create_post({
		title = "Introducing Crap Studio",
		slug = "introducing-crap-studio",
		excerpt = "We're excited to announce the launch of Crap Studio, a creative technology studio focused on building beautiful, performant digital experiences.",
		post_type = "article",
		author = admin.id,
		categories = { cat_culture.id },
		content = "<h2>Hello, World</h2><p>After years of freelancing and agency work, we've decided to build something of our own. Crap Studio is a creative technology studio that brings together design, engineering, and strategy under one roof.</p><p>We believe the best digital experiences come from teams that deeply understand both the craft of design and the power of technology. That's what we're building here.</p>",
		published_at = "2024-01-15T10:00:00Z",
		featured = true,
	})

	create_post({
		title = "Why We Chose Rust for Our Backend",
		slug = "why-rust-backend",
		excerpt = "A deep dive into our decision to use Rust for backend services, and the performance and reliability benefits we've seen.",
		post_type = "article",
		author = director.id,
		categories = { cat_dev.id },
		tags = { tag_rust.id, tag_perf.id },
		content = "<h2>The Case for Rust</h2><p>When we started building our internal tools, we evaluated several backend languages. Rust won us over with its combination of performance, safety, and developer experience.</p><h2>Real-World Results</h2><p>After six months of running Rust in production, our API response times dropped by 60% and memory usage decreased by 75% compared to our previous Node.js services.</p>",
		published_at = "2024-02-20T10:00:00Z",
	})

	create_post({
		title = "Building Accessible Design Systems",
		slug = "accessible-design-systems",
		excerpt = "How we approach accessibility in our design system, from color contrast to keyboard navigation and screen reader support.",
		post_type = "article",
		author = designer.id,
		categories = { cat_design.id },
		tags = { tag_a11y.id, tag_ds.id, tag_figma.id },
		content = "<h2>Accessibility as a Foundation</h2><p>Accessibility isn't an afterthought at Crap — it's baked into our design system from the ground up. Every component is tested against WCAG 2.1 AA standards.</p><h2>Color System</h2><p>Our color tokens are generated with contrast ratios in mind. Every foreground/background combination in our palette meets at least 4.5:1 contrast ratio for normal text.</p>",
		published_at = "2024-03-10T10:00:00Z",
		featured = true,
	})

	create_post({
		title = "The Art of Motion Design for the Web",
		slug = "motion-design-web",
		excerpt = "Principles of meaningful animation: when to animate, what to animate, and how to keep it performant.",
		post_type = "article",
		author = designer.id,
		categories = { cat_motion.id, cat_design.id },
		content = "<h2>Motion with Purpose</h2><p>Every animation should serve a purpose — guiding attention, providing feedback, or creating spatial awareness. Gratuitous animation is worse than no animation at all.</p><p>We follow three principles: <strong>purposeful</strong> (serves a UX goal), <strong>performant</strong> (60fps or bust), and <strong>accessible</strong> (respects prefers-reduced-motion).</p>",
		published_at = "2024-04-05T10:00:00Z",
	})

	create_post({
		title = "Case Study: Nova Financial Rebrand",
		slug = "case-study-nova-rebrand",
		excerpt = "A behind-the-scenes look at how we transformed Nova Financial's brand identity from traditional banking to modern fintech.",
		post_type = "case_study",
		author = admin.id,
		categories = { cat_brand.id },
		tags = { tag_fintech.id, tag_figma.id },
		related_content = { { collection = "projects", value = proj_nova_rebrand.id } },
		content = "<h2>From Brief to Launch</h2><p>The Nova Financial rebrand was our biggest brand project to date. Starting with extensive stakeholder interviews, we mapped the gap between their current perception and aspirational positioning.</p><h2>Research Phase</h2><p>We conducted 40 user interviews, analyzed 15 competitors, and ran 3 rounds of concept testing before finalizing the direction.</p>",
		published_at = "2024-09-15T10:00:00Z",
		featured = true,
	})

	create_post({
		title = "WebGL Performance Optimization Guide",
		slug = "webgl-performance-guide",
		excerpt = "Practical techniques for keeping WebGL applications smooth on mid-range hardware.",
		post_type = "article",
		author = dev.id,
		categories = { cat_dev.id },
		tags = { tag_webgl.id, tag_perf.id },
		content = "<h2>GPU Budget Management</h2><p>The key to performant WebGL is understanding your GPU budget. On mobile devices, you're often limited to ~16ms per frame. Here's how we keep our 3D experiences smooth across devices.</p>",
		published_at = "2024-05-20T10:00:00Z",
	})

	create_post({
		title = "Design Tokens: The Bridge Between Design and Code",
		slug = "design-tokens-bridge",
		excerpt = "How design tokens create a single source of truth for your brand across platforms and technologies.",
		post_type = "article",
		author = designer.id,
		categories = { cat_design.id, cat_dev.id },
		tags = { tag_ds.id, tag_figma.id },
		content = "<h2>What Are Design Tokens?</h2><p>Design tokens are the atomic values of your design system — colors, spacing, typography, shadows. They're the contract between design and engineering.</p>",
		published_at = "2024-06-12T10:00:00Z",
	})

	create_post({
		title = "Our Approach to UX Research",
		slug = "approach-ux-research",
		excerpt = "How we combine qualitative and quantitative research methods to drive design decisions.",
		post_type = "article",
		author = editor.id,
		categories = { cat_strategy.id, cat_design.id },
		tags = { tag_ux.id },
		content = "<h2>Research-Driven Design</h2><p>At Crap, every project starts with understanding the people we're designing for. Our research process combines interviews, surveys, analytics, and usability testing.</p>",
		published_at = "2024-07-08T10:00:00Z",
	})

	create_post({
		title = "The Future of AI in Design Tools",
		slug = "ai-design-tools-future",
		excerpt = "Exploring how AI is changing the design landscape and what it means for creative professionals.",
		post_type = "article",
		author = admin.id,
		categories = { cat_design.id, cat_strategy.id },
		tags = { tag_ai.id, tag_figma.id },
		content = "<h2>AI as a Creative Partner</h2><p>AI won't replace designers, but designers who use AI will replace those who don't. Here's how we're integrating AI tools into our creative workflow.</p>",
		published_at = "2024-08-15T10:00:00Z",
	})

	create_post({
		title = "Building HIPAA-Compliant Web Applications",
		slug = "hipaa-compliant-web-apps",
		excerpt = "Lessons learned from building healthcare software that meets strict compliance requirements.",
		post_type = "article",
		author = director.id,
		categories = { cat_dev.id },
		tags = { tag_health.id, tag_rust.id },
		content = "<h2>Security by Design</h2><p>HIPAA compliance isn't just about encryption — it's about building a security-first culture into every layer of your application.</p>",
		published_at = "2024-10-01T10:00:00Z",
	})

	create_post({
		title = "E-Commerce Performance: Lessons from Apex Retail",
		slug = "ecommerce-performance-lessons",
		excerpt = "What we learned building a platform that handles 50K concurrent shoppers with sub-second page loads.",
		post_type = "case_study",
		author = director.id,
		categories = { cat_dev.id },
		tags = { tag_perf.id, tag_ecom.id, tag_react.id },
		related_content = { { collection = "projects", value = proj_apex_ecom.id } },
		content = "<h2>Performance is a Feature</h2><p>Every 100ms of latency costs 1% of sales. For Apex Retail, we obsessed over every millisecond.</p>",
		published_at = "2024-11-10T10:00:00Z",
	})

	create_post({
		title = "Photography Tips for Product Shoots",
		slug = "photography-product-shoots",
		excerpt = "Professional product photography techniques that make your products shine.",
		post_type = "article",
		author = photographer.id,
		categories = { cat_design.id },
		content = "<h2>Lighting is Everything</h2><p>Good product photography starts with lighting. Here are the setups we use most often at the studio.</p>",
		published_at = "2024-12-05T10:00:00Z",
	})

	create_post({
		title = "Year in Review: 2024",
		slug = "year-in-review-2024",
		excerpt = "Looking back on a transformative year: 15 projects shipped, 3 new team members, and a whole lot of learning.",
		post_type = "article",
		author = admin.id,
		categories = { cat_culture.id },
		content = "<h2>What a Year</h2><p>2024 was our most ambitious year yet. We shipped 15 projects, grew the team from 3 to 6, and established ourselves as a go-to studio for design-forward technology.</p>",
		published_at = "2025-01-10T10:00:00Z",
		featured = true,
	})

	create_post({
		title = "Introduction to Creative Coding with Three.js",
		slug = "creative-coding-threejs",
		excerpt = "Getting started with Three.js for creative coders: from basic scenes to interactive experiences.",
		post_type = "article",
		author = dev.id,
		categories = { cat_dev.id, cat_motion.id },
		tags = { tag_webgl.id },
		content = "<h2>Your First Scene</h2><p>Three.js makes WebGL accessible. In this guide, we'll build an interactive 3D scene from scratch.</p>",
		published_at = "2025-02-14T10:00:00Z",
	})

	create_post({
		title = "Why Every Brand Needs a Design System",
		slug = "every-brand-needs-design-system",
		excerpt = "Design systems aren't just for big tech companies. Here's why your brand needs one too.",
		post_type = "article",
		author = designer.id,
		categories = { cat_design.id, cat_strategy.id },
		tags = { tag_ds.id },
		content = "<h2>Consistency at Scale</h2><p>A design system is the single source of truth for your brand's visual language. It speeds up development, ensures consistency, and reduces design debt.</p>",
		published_at = "2025-03-20T10:00:00Z",
	})

	create_post({
		title = "Awwwards: Site of the Day Breakdown",
		slug = "awwwards-site-of-the-day",
		excerpt = "Our Nova Financial project won Awwwards SOTD. Here's the technical breakdown.",
		post_type = "link",
		author = dev.id,
		external_url = "https://www.awwwards.com/sites/example",
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_fintech.id, tag_webgl.id },
		published_at = "2025-04-01T10:00:00Z",
	})

	create_post({
		title = "Conference Talk: The State of Web Animation",
		slug = "talk-state-web-animation",
		excerpt = "Taylor's talk at CSS Day 2025 on the current state and future of web animation.",
		post_type = "video",
		author = designer.id,
		external_url = "https://youtube.com/watch?v=example",
		categories = { cat_motion.id },
		published_at = "2025-05-15T10:00:00Z",
	})

	create_post({
		title = "Migrating from Node.js to Rust: A Practical Guide",
		slug = "nodejs-to-rust-migration",
		excerpt = "Step-by-step guide for teams considering a migration from Node.js to Rust for backend services.",
		post_type = "article",
		author = director.id,
		categories = { cat_dev.id },
		tags = { tag_rust.id, tag_perf.id },
		content = "<h2>When to Migrate</h2><p>Not every service needs Rust. Here's our framework for deciding when a migration makes sense and how to execute it incrementally.</p>",
		published_at = "2025-07-20T10:00:00Z",
	})

	create_post({
		title = "The Business Case for Accessibility",
		slug = "business-case-accessibility",
		excerpt = "Accessibility isn't just the right thing to do — it's good business. Here are the numbers to prove it.",
		post_type = "article",
		author = editor.id,
		categories = { cat_strategy.id },
		tags = { tag_a11y.id },
		content = "<h2>Beyond Compliance</h2><p>Companies that prioritize accessibility see 28% higher revenue, 2x user satisfaction scores, and significantly broader market reach.</p>",
		published_at = "2025-09-10T10:00:00Z",
	})

	create_post({
		title = "Studio Update: Q1 2026",
		slug = "studio-update-q1-2026",
		excerpt = "What we've been up to: new projects, team growth, and what's coming next.",
		post_type = "article",
		author = admin.id,
		categories = { cat_culture.id },
		content = "<h2>Busy Quarter</h2><p>Q1 2026 has been our busiest yet. We kicked off 3 new projects, spoke at 2 conferences, and are growing the team again.</p>",
		published_at = "2026-02-01T10:00:00Z",
	})

	create_post({
		title = "Designing for Dark Mode: Beyond Color Inversion",
		slug = "designing-dark-mode",
		excerpt = "Dark mode is more than swapping black and white. Here's our approach to dark themes that feel intentional.",
		post_type = "article",
		author = designer.id,
		categories = { cat_design.id },
		tags = { tag_ds.id },
		content = "<h2>Dark Mode Done Right</h2><p>Good dark mode design requires rethinking elevation, contrast, and color saturation — not just flipping a switch.</p>",
		published_at = "2025-10-15T10:00:00Z",
	})

	create_post({
		title = "How We Run Remote Design Sprints",
		slug = "remote-design-sprints",
		excerpt = "Adapting the Google Ventures design sprint for fully remote teams.",
		post_type = "article",
		author = editor.id,
		categories = { cat_strategy.id, cat_design.id },
		tags = { tag_ux.id },
		content = "<h2>Five Days, Fully Remote</h2><p>We've run over 20 remote design sprints. Here's the process, tools, and tips we've refined along the way.</p>",
		published_at = "2025-11-01T10:00:00Z",
	})

	create_post({
		title = "SVG Animation Techniques for the Modern Web",
		slug = "svg-animation-techniques",
		excerpt = "From SMIL to CSS to GSAP — a comprehensive guide to animating SVGs on the web.",
		post_type = "article",
		author = dev.id,
		categories = { cat_dev.id, cat_motion.id },
		content = "<h2>The SVG Animation Landscape</h2><p>SVG offers unique animation possibilities that CSS and canvas can't match. Here's how we leverage them for interactive illustrations and UI flourishes.</p>",
		published_at = "2025-12-10T10:00:00Z",
	})

	create_post({
		title = "Client Spotlight: Greenfield Energy Dashboard",
		slug = "client-spotlight-greenfield",
		excerpt = "Building a real-time energy monitoring dashboard for a renewable energy startup.",
		post_type = "case_study",
		author = director.id,
		categories = { cat_dev.id, cat_design.id },
		tags = { tag_react.id, tag_perf.id },
		content = "<h2>Real-Time at Scale</h2><p>Greenfield needed to visualize data from 10,000 sensors in real-time. We built a dashboard that handles it with sub-100ms updates.</p>",
		published_at = "2026-01-05T10:00:00Z",
	})

	create_post({
		title = "Typography on the Web: A Practical Guide",
		slug = "web-typography-guide",
		excerpt = "Choosing, loading, and fine-tuning type for the web — from font selection to variable fonts.",
		post_type = "article",
		author = designer.id,
		categories = { cat_design.id },
		tags = { tag_ds.id },
		content = "<h2>Type Matters</h2><p>Typography is the backbone of any design system. Here's everything we've learned about making type look great and load fast on the web.</p>",
		published_at = "2026-02-20T10:00:00Z",
	})

	-- ========================
	-- PAGES (4)
	-- ========================
	crap.collections.create("pages", {
		title = "Home",
		slug = "home",
		content = {
			{
				_block_type = "hero",
				heading = "We design and build digital experiences",
				subheading = "Crap Studio is a creative technology practice specializing in brand, product, and web.",
				cta_text = "See our work",
				cta_url = "/projects",
			},
			{
				_block_type = "services_list",
				heading = "What we do",
				services = { svc_brand.id, svc_web.id, svc_ux.id, svc_motion.id, svc_consult.id },
			},
		},
		template = "landing",
		show_in_nav = true,
		nav_order = 1,
	}, opts)

	crap.collections.create("pages", {
		title = "About",
		slug = "about",
		content = {
			{
				_block_type = "richtext",
				body = "<h2>Our Story</h2><p>Crap Studio was founded in 2024 with a simple belief: the best digital experiences come from teams that deeply understand both design and technology. We're a small, senior team that partners with ambitious companies to build products people love.</p>",
			},
			{
				_block_type = "team_grid",
				heading = "Meet the team",
				members = { admin.id, director.id, editor.id, designer.id, dev.id, photographer.id },
			},
		},
		template = "default",
		show_in_nav = true,
		nav_order = 2,
	}, opts)

	crap.collections.create("pages", {
		title = "Contact",
		slug = "contact",
		content = {
			{
				_block_type = "two_column",
				left = "<h2>Get in touch</h2><p>Have a project in mind? We'd love to hear about it. Drop us a line and we'll get back to you within 24 hours.</p><p><strong>Email:</strong> hello@crap.studio<br/><strong>Phone:</strong> +1 555 CRAP</p>",
				right = "<h2>Visit us</h2><p>Crap Studio<br/>123 Creative Ave, Suite 400<br/>San Francisco, CA 94105</p><p>Monday - Friday, 9am - 6pm PST</p>",
			},
		},
		template = "default",
		show_in_nav = true,
		nav_order = 4,
	}, opts)

	crap.collections.create("pages", {
		title = "Careers",
		slug = "careers",
		content = {
			{
				_block_type = "richtext",
				body = "<h2>Join the Team</h2><p>We're always looking for talented people who are passionate about design and technology. Check out our open positions below.</p>",
			},
			{
				_block_type = "cta_banner",
				heading = "Don't see a fit?",
				description = "We're always interested in hearing from talented people. Send us your portfolio and a note about what excites you.",
				button_text = "Say hello",
				button_url = "/contact",
			},
		},
		template = "default",
		show_in_nav = true,
		nav_order = 5,
	}, opts)

	-- ========================
	-- EVENTS (5)
	-- ========================
	crap.collections.create("events", {
		title = "Crap Design Meetup #12",
		slug = "design-meetup-12",
		description = "<p>Monthly design meetup hosted at Crap Studio. This month: <strong>Design Systems in Practice</strong> with talks from our team and guest speakers.</p>",
		start_date = "2026-03-15T18:00:00Z",
		end_date = "2026-03-15T21:00:00Z",
		online = false,
		location = {
			venue_name = "Crap Studio",
			address = "123 Creative Ave, Suite 400",
			city = "San Francisco",
			country = "USA",
		},
		speakers = { admin.id, designer.id },
		categories = { cat_design.id },
		registration_url = "https://meetup.com/example",
		max_attendees = 50,
	}, opts)

	crap.collections.create("events", {
		title = "WebGL Workshop: Creative Coding Fundamentals",
		slug = "webgl-workshop",
		description = "<p>Hands-on workshop covering the fundamentals of creative coding with Three.js and WebGL. Bring your laptop!</p>",
		start_date = "2026-04-10T10:00:00Z",
		end_date = "2026-04-10T16:00:00Z",
		online = true,
		event_url = "https://zoom.us/example",
		speakers = { dev.id },
		categories = { cat_dev.id, cat_motion.id },
		registration_url = "https://eventbrite.com/example",
		max_attendees = 100,
		registration_deadline = "2026-04-08T23:59:00Z",
	}, opts)

	crap.collections.create("events", {
		title = "CSS Day 2026 - Taylor Kim Speaking",
		slug = "css-day-2026",
		description = "<p>Taylor is speaking at CSS Day 2026 about advanced animation techniques and the future of motion on the web.</p>",
		start_date = "2026-06-12T09:00:00Z",
		end_date = "2026-06-13T17:00:00Z",
		online = false,
		location = {
			venue_name = "Kompaszaal",
			address = "KNSM-laan 311",
			city = "Amsterdam",
			country = "Netherlands",
		},
		speakers = { designer.id },
		categories = { cat_motion.id },
	}, opts)

	crap.collections.create("events", {
		title = "Studio Open House",
		slug = "studio-open-house",
		description = "<p>Come visit our new studio space! Drinks, demos, and good conversation.</p>",
		start_date = "2026-05-01T17:00:00Z",
		end_date = "2026-05-01T20:00:00Z",
		online = false,
		location = {
			venue_name = "Crap Studio",
			address = "123 Creative Ave, Suite 400",
			city = "San Francisco",
			country = "USA",
		},
		categories = { cat_culture.id },
		max_attendees = 75,
	}, opts)

	crap.collections.create("events", {
		title = "Rust for Web Developers Webinar",
		slug = "rust-web-developers-webinar",
		description = "<p>Free webinar: Getting started with Rust for web development. Perfect for Node.js/Python developers curious about Rust.</p>",
		start_date = "2026-03-28T17:00:00Z",
		end_date = "2026-03-28T18:30:00Z",
		online = true,
		event_url = "https://meet.google.com/example",
		speakers = { director.id },
		categories = { cat_dev.id },
		max_attendees = 200,
	}, opts)

	-- ========================
	-- TESTIMONIALS (8)
	-- ========================
	crap.collections.create("testimonials", {
		author_name = "Michael Torres",
		author_title = "CEO, Nova Financial",
		company = "Nova Financial",
		quote = "Crap didn't just redesign our brand — they helped us reimagine who we are. The new identity has fundamentally changed how our customers perceive us.",
		rating = 5,
		project = proj_nova_rebrand.id,
		featured = true,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "Sarah Kim",
		author_title = "CTO, Pulse Health",
		company = "Pulse Health",
		quote = "The technical depth of the Crap team is remarkable. They understood our HIPAA requirements from day one and built a platform our patients trust.",
		rating = 5,
		project = proj_pulse_portal.id,
		featured = true,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "David Okonkwo",
		author_title = "VP Engineering, Apex Retail",
		company = "Apex Retail",
		quote = "50K concurrent users with sub-second page loads. Crap delivered on a promise that three other agencies couldn't.",
		rating = 5,
		project = proj_apex_ecom.id,
		featured = true,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "Lisa Chang",
		author_title = "Head of Product, Verde Education",
		company = "Verde Education",
		quote = "Working with Crap feels like having a senior tech team embedded in your company. They truly care about the product.",
		rating = 4,
		project = proj_verde_lms.id,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "James Wright",
		author_title = "CTO, Echo Media Group",
		company = "Echo Media Group",
		quote = "Even in the planning phase, Crap's strategic thinking has been invaluable. Can't wait to see the finished product.",
		rating = 5,
		project = proj_echo_cms.id,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "Amanda Foster",
		author_title = "CMO, TechStart Inc",
		company = "TechStart Inc",
		quote = "The brand identity Crap created for us has been instrumental in our Series A fundraising. Investors consistently comment on our polished presence.",
		rating = 5,
		featured = true,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "Robert Chen",
		author_title = "Director of Digital, Pacific Northwest Health",
		company = "Pacific Northwest Health",
		quote = "Crap's understanding of healthcare UX is unmatched. They balance compliance requirements with genuinely delightful user experiences.",
		rating = 4,
	}, opts)

	crap.collections.create("testimonials", {
		author_name = "Elena Vasquez",
		author_title = "Founder, Bloom Creative",
		company = "Bloom Creative",
		quote = "We hired Crap for a quick brand refresh and ended up with a complete transformation. Best investment we've made.",
		rating = 5,
	}, opts)

	-- ========================
	-- INQUIRIES (5)
	-- ========================
	crap.collections.create("inquiries", {
		name = "Jennifer Park",
		email = "jennifer@techcorp.com",
		company = "TechCorp",
		service = svc_web.id,
		budget_range = "50k_100k",
		message = "We're looking to rebuild our customer portal from scratch. Current platform is slow and outdated. Would love to discuss a modern approach with your team.",
		status = "qualified",
		assigned_to = director.id,
		internal_notes = "Strong lead. They have budget approval and a Q3 deadline.",
	}, opts)

	crap.collections.create("inquiries", {
		name = "Marcus Johnson",
		email = "marcus@startupxyz.io",
		company = "StartupXYZ",
		service = svc_brand.id,
		budget_range = "15k_50k",
		message = "Series A startup looking for complete brand identity. We're launching in 3 months and need a professional brand that stands out in the SaaS space.",
		status = "proposal",
		assigned_to = admin.id,
	}, opts)

	crap.collections.create("inquiries", {
		name = "Sophie Williams",
		email = "sophie@greenorg.org",
		company = "Green Foundation",
		phone = "+1 555 987 6543",
		service = svc_ux.id,
		budget_range = "5k_15k",
		message = "Non-profit looking for UX audit of our donation platform. We're seeing high drop-off rates during checkout.",
		status = "new",
	}, opts)

	crap.collections.create("inquiries", {
		name = "Tom Bradley",
		email = "tom@example.com",
		budget_range = "under_5k",
		message = "Looking for someone to design a logo for my food truck. Love your style!",
		status = "contacted",
		internal_notes = "Budget too low for our minimum engagement. Referred to freelancer network.",
	}, opts)

	crap.collections.create("inquiries", {
		name = "Yuki Tanaka",
		email = "yuki@globalretail.jp",
		company = "Global Retail Japan",
		service = svc_web.id,
		budget_range = "over_100k",
		message = "We need to rebuild our e-commerce platform for the Japanese market. Looking for a team experienced with internationalization and high-traffic applications.",
		status = "new",
		metadata = '{"utm_source":"google","utm_medium":"cpc","utm_campaign":"web-dev-2026","referrer":"https://google.com"}',
	}, opts)

	-- ========================
	-- GLOBALS
	-- ========================
	crap.globals.update("site_settings", {
		site_name = "Crap Studio",
		tagline = "Design. Build. Launch.",
		contact_email = "hello@crap.studio",
		phone = "+1 555 CRAP",
		address = "123 Creative Ave, Suite 400\nSan Francisco, CA 94105",
		primary_color = "#2563eb",
		secondary_color = "#7c3aed",
		social = {
			github = "https://github.com/crap-studio",
			twitter = "https://twitter.com/crapstudio",
			linkedin = "https://linkedin.com/company/crap-studio",
			instagram = "https://instagram.com/crapstudio",
		},
	}, opts)

	crap.globals.update("navigation", {
		main_nav = {
			{ label = "Work", url = "/projects", children = {
				{ label = "All Projects", url = "/projects" },
				{ label = "Case Studies", url = "/blog?type=case_study" },
			} },
			{ label = "Services", url = "/services" },
			{ label = "Blog", url = "/blog" },
			{ label = "About", url = "/about" },
			{ label = "Contact", url = "/contact" },
		},
	}, opts)

	crap.globals.update("footer", {
		copyright_text = "Crap Studio. All rights reserved.",
		show_social_links = true,
	}, opts)

	crap.log.info("Crap Studio seed complete: 6 users, 5 clients, 6 categories, 12 tags, 5 services, 10 projects, 20 posts, 4 pages, 5 events, 8 testimonials, 5 inquiries, 3 globals")
end

function M.down()
	local collections = {
		"inquiries",
		"testimonials",
		"events",
		"pages",
		"posts",
		"projects",
		"services",
		"clients",
		"tags",
		"categories",
		"media",
		"users",
	}

	local opts = { overrideAccess = true }

	for _, collection in ipairs(collections) do
		local result = crap.collections.find(collection, { limit = 1000, overrideAccess = true })
		if result and result.documents then
			for _, doc in ipairs(result.documents) do
				crap.collections.delete(collection, doc.id, opts)
			end
		end
	end

	crap.log.info("Crap Studio seed removed")
end

return M
