# Pending prompts

1.  What if user id is not an i64 but rather a custom user model with uuid for id or pk? It should be picking the primary key from the tables. Most tables have a primary key, we can enforce and say each table should have a pk. With this hardcode, will it work in a case where use id is a uuid or even a string? (This is in permissions apps)

And from the above, we need to update the docs on how to reference User model, next we need to show the dev how to write custom user model. We only have email, username and password. What if the dev instead of extending by 1by1 using another model like Account, they decide to
write their own model. So like in django where you declare the Custom User Model, you say what is the username field for reference, we should have something ofsought. It should have been worked on, maybe its pending
somewhere/home/dalmas/E/projects/umbra/plugins/umbra-permissions/src/models.rs


 The right scoping:
  - Quick fix this turn: make perm's user_id: String so it accepts ANY stringifiable PK (i64, UUID, slug). Callers convert at the boundary.
  - Bigger refactor (separate session): lift UserModel::id() -> i64 to a generic Self::Pk. Touches auth/sessions/admin — not a "quick" change. -> THIS IS PENDING
  - Docs update this turn: clarify the "login handle" abstraction in auth.mdx (it's not necessarily the username — same Django's USERNAME_FIELD story), and document the perm-plugin's String user_id convention.
