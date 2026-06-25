# umbra trademark and branding policy

The **umbra** source code is open source under [MIT](LICENSE-MIT) **OR** [Apache-2.0](LICENSE-APACHE). You can fork it, modify it, ship it, and sell it — that freedom is the point of the license, and nothing here takes it away.

This document is about something the code license deliberately does *not* cover: the **name "umbra", the project's logo, and its visual identity**. Open-source licenses grant rights to *copyright* (the code); they do not grant rights to *trademarks* (the name people use to find the official project). The Apache-2.0 license makes this explicit in its Section 6. This policy spells out what that means in practice, in plain language.

The goal is narrow and friendly: keep the name "umbra" pointing at *this* project so users don't get confused about what's official and what isn't. It is **not** meant to discourage forks, plugins, integrations, or honest discussion.

## What you can always do (no permission needed)

- **Use, fork, modify, and redistribute the code** under MIT or Apache-2.0.
- **Say your software is "built with umbra", "powered by umbra", or "compatible with umbra".** Nominative, factual references are fine and encouraged.
- **Write articles, tutorials, talks, and books** about umbra and use the name to refer to it.
- **Publish third-party plugins, tools, and extensions.** Name them descriptively — `umbra-stripe`, `umbra-graphql`, `awesome-umbra` — so it's clear they're *for* umbra, not *the official* umbra. (See naming guidance below.)
- **Run an unmodified copy of umbra as a service.**

## What needs permission

Ask first (open an issue or email the maintainer) before you:

- **Use "umbra" as the name of a different product, framework, or company**, or in any way that implies your project *is* the official umbra or is endorsed by it.
- **Publish a redistribution of a *modified* umbra under the "umbra" name** in a way that could be mistaken for the official release. Fork freely — but rename a hard fork so users know it diverged (e.g. "Nimbus, a fork of umbra"), the same way Chromium is a renamed Chrome and MariaDB a renamed MySQL.
- **Use the umbra logo or visual identity** in your own product's branding, marketing, or merchandise.
- **Register "umbra" (or a confusingly similar mark) as a trademark, domain, crates.io name, or social handle** in a way that impersonates or competes with the official project's identity.

## Naming third-party crates

Because all official crates publish under the `umbra-*` prefix (`umbra-core`, `umbra-rest`, `umbra-admin`, …), a third-party crate named `umbra-foo` can read as official. To keep the boundary clear:

- **Preferred:** put your name or a descriptor first — `acme-umbra-stripe`, or a standalone name like `tenebris`.
- **Acceptable:** the `umbra-` prefix *with* a clear "unofficial / community" note in the crate description and README.
- **Not OK:** an `umbra-*` name that mimics a planned official crate and ships as if it were maintained by the umbra project.

When in doubt, ask — we'd rather say "yes, go ahead" early than have to untangle confusion later.

## Why this exists

A permissive code license without a trademark policy is the historically-proven recipe for "someone ships a buggy/abandoned/malicious fork under your name and your users blame you." Reserving only the *name* — while leaving the *code* maximally free — is the standard open-source resolution (Rust, Python, Mozilla, Linux, and most large projects do exactly this). It protects users far more than it restricts contributors.

## Questions

This policy is intentionally light. If something you want to do isn't clearly covered, open an issue or email **dalmasogembo@gmail.com**. Good-faith use of the name to talk about, build on, and contribute to umbra is always welcome.
