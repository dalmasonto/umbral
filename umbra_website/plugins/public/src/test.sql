 SELECT "id", "public_id",
     "name", "slug", "crate_name", "author", "short_description", "full_content",
     "installation_commands", "setup_notes", "docs_url", "source_url", "issue_tracker_url",
     "version", "license", "status", "maturity", "audit_status", "security_status", "source",
     "moderation", "featured", "display_order", "github_stars", "downloads", "metadata", "created_at",
     "updated_at", "deleted_at",
     (SELECT COUNT(*) FROM "plugin_directory_plugin_comment" WHERE "plugin_directory_plugin_comment"."plugin" = "plugin_directory_plugin"."id") AS "comment_set_count"
FROM "plugin_directory_plugin"
WHERE "source" <> ? AND "moderation" = ? AND "deleted_at" IS NULL
