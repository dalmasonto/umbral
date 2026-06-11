# Umbra Framework Website

## Goal

Build the official Umbra website as an Umbra project, using Umbra itself as the proof of the framework.

The website should not be a thin landing page. It should be a complete information hub for developers who want to understand what Umbra is, what is already usable, what is coming next, how apps and plugins work, what can be extended, and what real projects are using it.

The site should make Umbra feel more transparent than other Rust web framework sites. A visitor should be able to answer practical questions without hunting through the repository: what features exist, what is planned, which prebuilt plugins exist, which community plugins are available, what developers are saying about each plugin, how safe a plugin is, how to start a project, how to add an app, and where to read deeper examples.

## Positioning

The homepage should not lead with a generic familiar-framework-for-Rust message as the whole pitch. That space overlaps with cot.rs, and Umbra needs a sharper public position.

The stronger Umbra message is that it is a highly modular Rust web framework where the batteries are real plugins. Official batteries should be presented as swappable pieces rather than as one fixed monolith.

The site can still explain the familiar workflow: models, migrations, admin, forms, REST, and background work. The public framing should emphasize that Umbra gives that productive app shape while keeping each major capability behind the same plugin boundary a third-party developer can use.

The homepage should communicate three ideas quickly:

1. Umbra gives Rust developers a complete app framework, not only HTTP routing.
2. Umbra's batteries are prebuilt plugins that can be enabled, replaced, or extended.
3. The website itself is built with Umbra, using the same project, app, migration, form, admin, and plugin systems it documents.

## Project Rules

The website project directory should be created at the repository root as `umbra_website`.

Use `umbra-cli startproject` to start the website project.

Use `startapp` for each website app or plugin area. Models should live in the generated plugin folders, not in `main.rs`.

`main.rs` should stay small. It should wire settings, plugins, routes, and app startup. It should not contain website data models.

Do not put an auto-migrate routine into `main.rs`. The website should use the normal production flow: make migrations, review them, then migrate.

Use `makemigrations` and `migrate` for schema changes. The website should dogfood the declare, migrate, change, migrate loop.

Use Tailwind CSS for styling, compiled into static CSS for the website.

Use Umbra Forms for every public form on the site. Contact forms, plugin submission, review submission, plugin reporting, showcase submission, and account connection flows should all use Forms.

Content, features, plugins, social links, reviews, showcase entries, and moderation data should be managed through the database and the admin, not hardcoded into HTML templates.

## Information Architecture

The primary navigation should include Home, Features, Prebuilt Plugins, Plugin Directory, Docs, Blog, Showcase, Reviews, Security, Community, and Changelog.

Home should introduce Umbra, show the built-with-Umbra proof point, surface the strongest framework capabilities, and link into the deeper sections.

Features should explain framework-level capabilities that are not better represented as individual prebuilt plugins.

Prebuilt Plugins should explain official Umbra plugins maintained by the Umbra team.

Plugin Directory should list first-party plugins, community plugins, experimental plugins, deprecated plugins, flagged plugins, and plugin-specific discussion threads.

Docs should link into the existing user documentation and expose curated learning paths: quick start, project structure, models and migrations, forms, plugins are apps, admin, auth, permissions, REST, deployment, and building a reusable plugin.

Blog should publish release notes, design notes, tutorials, plugin announcements, migration guides, security advisories, and "building Umbra with Umbra" posts.

Showcase should list websites and applications using Umbra.

Reviews should show verified developer reviews of the Umbra project.

Security should explain plugin safety, malicious plugin reporting, audit status, responsible disclosure, and ecosystem maintainer workflows.

Community should expose GitHub, Discord, Reddit, X, RSS, Sentinmail newsletter subscription, documentation, and any future channels from database-managed social links.

## Feature Catalog

The feature list should be managed from the database.

No feature cards should be hardcoded into HTML.

The feature catalog should focus on framework-wide capabilities such as project scaffolding, app scaffolding, model declaration, migration workflow, query ergonomics, multi-database direction, admin integration points, form handling, routing, template rendering, settings, static assets, deployment shape, and developer experience.

Prebuilt plugin capabilities should not clutter the main feature catalog. They should live under their plugin detail pages and roll up into the Prebuilt Plugins section.

Each feature should have a name, slug, short summary, full description, category, status, maturity, docs URL, example URL, related plugin when applicable, release target, display order, and visibility flag.

Feature statuses should make it clear what developers can rely on now and what is still planned.

Feature pages should explain how the feature works in Umbra, why it exists, and where to find the docs or example.

The homepage should show a curated subset of features. The full Features page should show the complete database-backed list.

The admin should let maintainers mark features as shipped, usable, experimental, in progress, planned, deferred, or deprecated without touching templates.

## Prebuilt Plugins

Prebuilt plugins are one of Umbra's strongest public arguments and deserve their own section.

This section should explain that official Umbra capabilities are delivered through the same plugin contract community developers can use.

Each prebuilt plugin should have its own database record, public page, status, documentation links, setup notes, compatibility notes, and feature list.

Each prebuilt plugin should track its individual features from the database. For example, the admin plugin can list dashboards, CRUD, filters, sheets, bulk actions, and preferences as plugin-owned features, while the REST plugin can list serializers, viewsets, routers, OpenAPI, pagination, filtering, and playground features.

Each plugin-owned feature should have a name, slug, description, status, maturity, release target, docs link, example link, display order, and visibility flag.

The plugin detail page should show the plugin's own feature tracker so users can see what is shipped, what is experimental, and what is planned inside that plugin.

The website should make swapping clear: an Umbra app can use the official plugin, replace it with another plugin, or build a project-specific plugin through `startapp`.

The seed command should seed default records for the official Umbra plugins and their initial feature lists.

The initial prebuilt plugin list should be generated from the existing top-level Umbra plugins, then maintained through the database.

## Community Plugin Directory

Umbra should expose the plugin ecosystem clearly on the website.

The plugin directory should include first-party plugins, community plugins, experimental plugins, deprecated plugins, and flagged plugins, but the public UI should distinguish official prebuilt plugins from community plugins.

Each plugin listing should include plugin name, slug, author, package or repository URL, short description, full rich text content, installation commands, setup notes, version, license, supported Umbra versions, supported database backends, documentation URL, source URL, issue tracker URL, categories, tags, status, audit status, security status, and last verified date.

Plugin install commands and setup notes should be stored as database content so maintainers can update them without changing templates.

A plugin detail page should explain what the plugin does, how to install it, how to configure it, what models it owns, what migrations it creates, what routes or commands it adds, what features it provides, what developers are saying about it, and whether it is first-party or community-maintained.

Plugin search should support filters by category, status, audit status, compatibility, author, and first-party versus community.

Plugin submission should require a connected GitHub account that is at least 3 years old.

The GitHub account age check is deferred until a GitHub OAuth plugin exists, but the website data model and moderation workflow should be designed around it now.

Until OAuth exists, plugin submission can be admin-only or manually reviewed from submitted form data.

## Plugin Comments And Discussions

Umbra should provide a central place for plugin-specific comments and discussions across official and community plugins.

This is a major ecosystem gap. In many framework ecosystems, there is no single place to read practical comments about plugins such as REST frameworks, multitenancy packages, admin extensions, task queues, or auth replacements. Umbra should make this visible from the beginning.

Every plugin detail page should have a discussion area where developers can leave usage notes, installation gotchas, compatibility reports, migration notes, questions, maintainer replies, and general feedback.

Plugin comments are not the same as developer reviews of Umbra itself, and they are not the same as malicious plugin reports. Reviews measure trust in the framework. Security reports go to an auditor workflow. Plugin comments are public, contextual discussion around a specific plugin.

Comments should be attached to a plugin record, not scattered through blog posts or external issue trackers.

Comments should support replies or threads so maintainers can answer questions in context.

Comments should support moderation states such as pending, visible, hidden, flagged, deleted, and locked.

Comments should support pinned maintainer notes so a plugin author or Umbra maintainer can surface important compatibility warnings or migration guidance.

Comments should support lightweight metadata such as plugin version, Umbra version, database backend, operating system, and whether the comment is a question, usage note, compatibility note, maintainer response, or migration note.

Comments should support abuse reporting and moderator review.

Comments should be searchable and filterable from plugin pages and from a cross-plugin discussion view.

Commenting can start as a normal Umbra Forms workflow with server-rendered updates after submit.

The data model should be designed so an SSE and WebSockets plugin can later add live interactions without redesigning comments.

The future live layer should support new comment updates, replies appearing without refresh, live moderation state changes, maintainer responses, and updated comment counts on plugin listings.

The initial version does not need live transport. It should store enough event and timestamp data for the SSE/WebSockets plugin to stream activity later.

## Blog And Content System

The content plugin from `examples/shop/plugins/content` should be copied or moved into the top-level `plugins/` directory as a reusable first-party content plugin.

That reusable content plugin should become the starting point for blog posts, pages, FAQ, navigation, media assets, redirects, site settings, contact messages, banners, and testimonials.

The Umbra website can extend the reusable content plugin when it needs website-specific models, but the generic content functionality should not be rebuilt from scratch inside `umbra_website`.

Blog posts should support draft, published, and scheduled states.

Blog posts should support authors, categories, tags, cover images, attachments, excerpts, SEO title, SEO description, reading time, featured status, and publish date.

The blog should support tutorials, release notes, design notes, plugin spotlights, security advisories, and community posts.

The blog should expose RSS or Atom feeds.

The blog should have search and filtering by tag, category, author, and post type.

The blog editor should use rich text content or markdown content, but the public planning requirement is simply that authors can publish long-form formatted content without editing templates.

## Social, Community, And Newsletter

Social links should not be hardcoded in templates. Store them in site settings or a dedicated social link model.

The initial social link set should include GitHub, Discord, Reddit, X, RSS, documentation, and newsletter.

Optional future social links can include YouTube, LinkedIn, Mastodon, Bluesky, and Matrix if the project starts using them.

Each social link should have a name, URL, icon key, display order, active flag, and optional description.

The footer, community page, blog sidebar, and homepage community section should all read from the same database-managed social links.

Newsletter subscription should point outward to Sentinmail instead of storing local subscribers in the Umbra website database.

The newsletter form can still use Umbra Forms for validation and user experience, but the submitted data should be sent to the Sentinmail subscribe endpoint documented at `https://docs.sentinmail.app/docs/v1.0.0/dev-guide/subscribe-page`.

The website should support either a hosted Sentinmail subscribe page link or a local Umbra Form that forwards to Sentinmail, depending on the final integration choice.

## Reviews And Trust

The website should include developer reviews for the Umbra project itself.

A developer review should require the reviewer to connect a GitHub account.

The connected GitHub account must be at least 1 year old before the user can leave a review.

The GitHub OAuth and account age verification are deferred because the OAuth plugin does not exist yet.

The rest of the review system should still be designed now: review model, moderation status, admin workflow, public display, and abuse prevention.

Each review should include developer name, GitHub username, optional avatar, rating, review title, review body, developer role, company or project type, Umbra version used, usage context, verified GitHub status, moderation status, and publish date.

Only one active review per GitHub account should be allowed.

Reviews should support moderation states such as pending, approved, rejected, hidden, and needs follow-up.

Reviews should not be anonymous on the public site. If a review influences trust in the framework, it should be tied to a visible developer identity.

The homepage should show a small curated set of approved reviews. The Reviews page should show the full approved list with filters by usage type, version, and rating.

## Websites Using Umbra

The website should have a "Websites using Umbra" section.

This section should list real sites, apps, dashboards, APIs, internal tools, and demos built with Umbra.

Each showcase entry should include project name, URL, owner or organization, short description, long case study content, screenshot or logo, project type, Umbra version, plugins used, database backend, deployment platform, launch date, source URL if public, and verification status.

Showcase entries should support statuses such as draft, submitted, verified, featured, archived, and rejected.

The homepage should show featured showcase entries. The full Showcase page should show the complete verified list.

Showcase submission should use Umbra Forms.

Showcase submission can be public later, but it should be moderated before publishing.

If GitHub OAuth is available later, showcase submitters can connect GitHub to prove ownership or maintainer identity.

The first showcase entry should be the Umbra website itself.

## Malicious Plugin Reporting

The website should have a serious plugin safety section.

Umbra plugins can run application code, define models, add routes, and access data. A malicious plugin could leak data, weaken authentication, add unsafe routes, exfiltrate secrets, or hide behavior inside migrations.

Every plugin page should have a visible way to report a security concern.

A malicious plugin report should collect the plugin, reporter identity, issue category, affected version, explanation, evidence links, reproduction notes, and whether the issue has been privately disclosed to the maintainer.

Voting that a plugin is malicious should require a connected GitHub account that is at least 2 years old.

The 2-year account gate is intended to reduce drive-by abuse and coordinated false reports.

The GitHub gate is deferred until OAuth exists, but the models and workflow should reserve room for it.

Reports should not instantly delist a plugin. They should create a moderation queue for plugin auditors.

Auditors should be able to mark a report as new, triaged, needs more evidence, confirmed, false positive, fixed, or advisory published.

Confirmed malicious plugins should show a warning on their directory listing and detail page.

The site should support ecosystem alerts for confirmed malicious plugins, including a security advisory blog post and a prominent warning on the plugin page.

There should be a dedicated role or permission group for plugin auditors and security maintainers.

## Forms To Build

The contact form should use Umbra Forms and store contact messages in the database.

The newsletter form should use Umbra Forms and forward valid submissions to Sentinmail.

The plugin submission form should use Umbra Forms and store plugin submissions for moderation.

The plugin report form should use Umbra Forms and store security reports for auditor review.

The plugin comment form should use Umbra Forms and store comments or replies for moderation and public display.

The developer review form should use Umbra Forms and store reviews for moderation.

The showcase submission form should use Umbra Forms and store site submissions for moderation.

Every public form should have server-side validation, friendly validation errors, spam resistance, and a clear moderation state where needed.

## Website Apps To Scaffold

Use `startapp` to create separate app or plugin areas for the website.

The content app should come from the reusable content plugin and handle blog, pages, FAQ, navigation, media, redirects, site settings, contact messages, banners, and testimonials.

The features app should own the framework feature catalog and release status data.

The prebuilt plugins app should own official plugin records and plugin-owned feature tracking.

The plugin directory app should own community plugin listings, plugin categories, plugin compatibility, plugin install content, plugin submissions, plugin comments, plugin discussion threads, and plugin status.

The reviews app should own verified developer reviews.

The showcase app should own websites and applications using Umbra.

The security app should own malicious plugin reports, advisory state, auditor workflow, plugin comment abuse reports, and plugin warnings.

The accounts app should be reserved for GitHub identity, OAuth connection, account age verification, and user trust gates once an OAuth plugin exists.

The community app can own social links, community resources, Sentinmail newsletter configuration, and external channel metadata if this is not handled entirely by site settings.

The search app can be added later if cross-site search becomes large enough to deserve its own boundary.

## Admin And Moderation

The admin should be the main tool for maintaining website content.

Maintainers should be able to manage framework features, prebuilt plugins, plugin-owned features, community plugins, plugin comments, plugin discussion moderation, blog posts, pages, social links, reviews, showcase entries, contact messages, security reports, advisories, and Sentinmail integration settings from the admin.

Moderation queues should exist for plugin submissions, plugin comments, plugin comment abuse reports, plugin reports, developer reviews, and showcase submissions.

The site should distinguish public content from submitted content. Submitted content should not appear publicly until approved.

Plugin auditors should have permission to triage security reports without needing full site administrator access.

Content editors should be able to manage blog and pages without needing plugin security permissions.

## Seed Data And Commands

The website should have a seed command for default website data.

The seed command should seed the first social links: GitHub, Discord, Reddit, X, RSS, documentation, and newsletter.

The seed command should seed the first framework feature categories: Core, ORM, Migrations, Templates, Forms, Project Structure, Developer Experience, Security, Deployment, and Plugin System.

The seed command should seed the initial framework feature list from the current Umbra capabilities and mark each one with an honest status.

The seed command should seed official prebuilt plugin records for the existing top-level Umbra plugins.

The seed command should seed each official plugin's initial feature list from the database model designed for plugin-owned features.

The seed command should seed default plugin comment categories such as question, usage note, compatibility note, migration note, maintainer response, and general feedback.

The seed command should seed the reusable content plugin once it is promoted from the shop example.

The seed command should seed the first showcase entry as the Umbra website itself.

The seed command should seed initial blog categories: Releases, Tutorials, Design Notes, Plugins, Security, Community.

The seed command should seed an initial "Why Umbra exists" post and a "Building the Umbra website with Umbra" post.

## Deferred Open Areas

GitHub OAuth is not available yet. Account connection, GitHub username verification, avatar import, repository ownership checks, and account age gates are deferred behind an OAuth plugin.

Developer reviews require GitHub accounts at least 1 year old, but enforcement is deferred until OAuth exists.

Plugin submission requires GitHub accounts at least 3 years old, but enforcement is deferred until OAuth exists.

Malicious plugin voting requires GitHub accounts at least 2 years old, but enforcement is deferred until OAuth exists.

The website should still model these workflows now so the OAuth plugin can be added later without redesigning the database.

SSE and WebSockets support is not required for the first plugin comments release. The comments system should work through normal form submissions first, then the future SSE/WebSockets plugin can add live comment streams, live reply updates, live moderation changes, and live plugin activity counters.

## Design And Styling Direction

The website should feel like a serious developer tool, not a generic SaaS landing page.

It should be dense enough for experienced developers but still approachable for someone evaluating Umbra for the first time.

The visual system should prioritize readable technical content, clear navigation, good comparison tables, strong search and filtering, and polished long-form documentation pages.

The first viewport should clearly communicate Umbra's identity, the plugin-first architecture, and the fact that the website is built with Umbra.

Use Tailwind CSS for layout, typography, forms, tables, badges, filters, and responsive behavior.

Avoid making the whole site a static marketing surface. The important sections should prove the framework: database-backed features, database-backed prebuilt plugins, database-backed plugin-owned feature trackers, database-backed community plugins, database-backed plugin comments, database-backed reviews, database-backed showcase, real forms, admin-managed content, migrations, and an external Sentinmail integration.

## Build Sequence After Review

First, make sure the Umbra core and CLI are capable of creating the project and apps needed for the website.

Use `umbra-cli startproject` to create the root-level `umbra_website` project.

Use `startapp` to create the website apps instead of putting models in `main.rs`.

Promote the shop content plugin into a reusable top-level plugin and wire it into the website.

Define the website-specific models in their app folders.

Add the seed command for default social links, framework features, official plugins, plugin-owned features, plugin comment categories, blog categories, and the first showcase entry.

Run `makemigrations`, review the generated migrations, and run `migrate`.

Build the Tailwind styling, templates, forms, plugin comments, admin configuration, moderation workflows, seed data, and Sentinmail newsletter integration.

Use the website itself as the first public proof that Umbra can build a real content-heavy framework site.

## Good features

1. Plugin ordering - since it might be hard to track installs as they happen through cargo add, it might be useful to use plugin reviews, rating, to order plugins by popularity or relevance. Actually to track plugin installs, we can provide an installation command like `umbra install <plugin>` that adds the plugin to the project using cargo add but sends a tracking event to the umbra backend. This means, whenever umbra cli is used to install a plugin, we instantiate like a config in the user root that is saved and signed by our server, its like an auth key but in this it will be specifically an identification key to track plugin installs from different devices. We can make this opt in only but this means we don't use installs as a metric. When we have a given device id which is only known to our server means that regardless the user installing the plugin 10x, it can only be recorded once till that code changes! Actually we can also use crates.io site `https://crates.io{crate_name}/{version}/downloads`
