# umbral-admin

Auto-generated CRUD admin for umbral models. Drop in `AdminPlugin` and every
registered model gets a list view, detail view, create form, edit form, and
delete action under `/admin/`.

## Development

In `dev` mode (the default) the admin loads Tailwind CSS from the CDN:

```html
<script src="https://cdn.tailwindcss.com?plugins=forms,container-queries"></script>
```

No Node.js installation is required for development.

## Building the production CSS

For production deployments the admin embeds a pre-compiled stylesheet at
`/admin/static/admin.css`. Build it once and commit the output:

```sh
cd plugins/umbral-admin/css
npm install
npm run build
```

The output lands at `plugins/umbral-admin/src/assets/admin.css` and is
embedded into the binary via the static assets route.

The Cargo build script (`build.rs`) will attempt to run this automatically if
`node_modules/` is present in the `css/` directory. If Node is not installed,
the build script emits a `cargo:warning` and skips the CSS build - the
framework still compiles and serves via the CDN in dev mode.

## Password management

Models with a `password_hash`-style column should use `AdminModel::password_field`:

```rust
AdminPlugin::default()
    .register(
        AdminModel::new("auth_user")
            .password_field("password_hash")
    )
```

This tells the admin to:
- Never render the raw hash as a form input.
- Show a "Password" + "Confirm password" pair on create forms.
- Show a "Change password" button on edit forms.

Custom user models follow the same pattern - supply the column name that
stores the argon2 hash.
