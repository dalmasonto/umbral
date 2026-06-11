# Keep this directory tracked even when admin.png is absent.
#
# To regenerate admin.png:
#   1. Start the dev server: cargo run -- serve
#   2. Log in as the superuser once to create a session.
#   3. From umbra_website/styles/ run:  npx playwright install chromium
#   4. Then:  npm run screenshot
#
# The hero on the home page layers admin.png on top of a static
# placeholder; if admin.png is missing, the placeholder stays visible.
