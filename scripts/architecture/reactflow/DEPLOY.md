# Deploying the architecture diagram

The `deploy-architecture` workflow (`.github/workflows/deploy-architecture.yml`)
builds this Vite + React Flow app and publishes it to GitHub Pages **in a
separate repository** — so it doesn't collide with the docs site that already
serves from this repo.

It runs automatically when anything under `scripts/architecture/reactflow/**`
changes on `main`, and can be triggered manually from the **Actions** tab.

## Why a separate repo needs extra setup

The default `GITHUB_TOKEN` is scoped to *this* repo and can't push to another
one. So the deploy step authenticates to the **target** repo with an SSH deploy
key (recommended — scoped to just that repo) or a Personal Access Token. Both
the target repo and the credential are read from a repo **variable** + **secret**,
so nothing is hardcoded in the workflow.

## One-time setup (deploy key — recommended)

1. **Create the target repo**, e.g. `dalmasonto/umbral-architecture` (public).

2. **Generate a deploy key** (no passphrase):

   ```bash
   ssh-keygen -t ed25519 -f arch_deploy -N "" -C "umbral-architecture-deploy"
   # → arch_deploy (private)  +  arch_deploy.pub (public)
   ```

3. **Target repo** → Settings → **Deploy keys** → *Add deploy key* → paste
   `arch_deploy.pub`, tick **Allow write access**.

4. **This repo** (`umbral`) → Settings → Secrets and variables → **Actions**:
   - **Secrets** → *New repository secret*
     `ARCH_PAGES_DEPLOY_KEY` = the full contents of the **private** `arch_deploy`.
   - **Variables** → *New repository variable*
     `ARCH_PAGES_REPO` = `dalmasonto/umbral-architecture`.

5. Delete the local key files (`rm arch_deploy arch_deploy.pub`).

6. Run the workflow once (push a change or dispatch it). The first run creates
   the `gh-pages` branch in the target repo.

7. **Target repo** → Settings → **Pages** → *Deploy from a branch* →
   `gh-pages` / `root`. The site resolves at
   `https://<owner>.github.io/<target-repo>/`.

## PAT alternative

Instead of a deploy key: create a fine-grained PAT with **Contents: write** on
the target repo, store it as the `ARCH_PAGES_TOKEN` secret in this repo, then in
the workflow comment out `deploy_key:` and uncomment `personal_token:`.

## Notes

- The app's Vite `base` is `./` (relative), so it works at any Pages sub-path
  without a repo-name base — no config change needed per target repo.
- If `ARCH_PAGES_REPO` isn't set, the job is skipped (no red X on forks).
