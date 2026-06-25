import type { OpenAPIV3 } from "openapi-types";

export const mockSpec: OpenAPIV3.Document = {
  openapi: "3.0.3",
  info: {
    title: "Umbral Blog API",
    version: "1.0.0",
    description: "A demo REST API powering the Umbral playground. Built with umbral-rest.",
  },
  servers: [{ url: "/api" }],
  paths: {
    "/posts": {
      get: {
        operationId: "listPosts",
        summary: "List all posts",
        description: "Returns a paginated list of blog posts.",
        tags: ["Posts"],
        parameters: [
          {
            name: "page",
            in: "query",
            schema: { type: "integer", default: 1 },
          },
          {
            name: "limit",
            in: "query",
            schema: { type: "integer", default: 20 },
          },
          {
            name: "search",
            in: "query",
            schema: { type: "string" },
          },
        ],
        responses: {
          "200": {
            description: "List of posts",
            content: {
              "application/json": {
                schema: {
                  type: "object",
                  properties: {
                    results: {
                      type: "array",
                      items: { $ref: "#/components/schemas/Post" },
                    },
                    count: { type: "integer" },
                    next: { type: "string", nullable: true },
                    previous: { type: "string", nullable: true },
                  },
                },
              },
            },
          },
        },
      },
      post: {
        operationId: "createPost",
        summary: "Create a new post",
        tags: ["Posts"],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/PostInput" },
            },
          },
        },
        responses: {
          "201": { description: "Post created" },
          "422": { description: "Validation error" },
        },
      },
    },
    "/posts/{id}": {
      get: {
        operationId: "getPost",
        summary: "Get a post by ID",
        tags: ["Posts"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        responses: {
          "200": { description: "Post found" },
          "404": { description: "Post not found" },
        },
      },
      put: {
        operationId: "updatePost",
        summary: "Update a post",
        tags: ["Posts"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/PostInput" },
            },
          },
        },
        responses: {
          "200": { description: "Post updated" },
          "404": { description: "Post not found" },
        },
      },
      patch: {
        operationId: "partialUpdatePost",
        summary: "Partially update a post",
        tags: ["Posts"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/PostInput" },
            },
          },
        },
        responses: {
          "200": { description: "Post updated" },
          "404": { description: "Post not found" },
        },
      },
      delete: {
        operationId: "deletePost",
        summary: "Delete a post",
        tags: ["Posts"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        responses: {
          "204": { description: "Post deleted" },
          "404": { description: "Post not found" },
        },
      },
    },
    "/users": {
      get: {
        operationId: "listUsers",
        summary: "List all users",
        tags: ["Users"],
        parameters: [
          {
            name: "role",
            in: "query",
            schema: { type: "string", enum: ["admin", "editor", "reader"] },
          },
          {
            name: "is_active",
            in: "query",
            schema: { type: "boolean" },
          },
        ],
        responses: {
          "200": { description: "List of users" },
        },
      },
      post: {
        operationId: "createUser",
        summary: "Create a new user",
        tags: ["Users"],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/UserInput" },
            },
          },
        },
        responses: {
          "201": { description: "User created" },
          "422": { description: "Validation error" },
        },
      },
    },
    "/users/{id}": {
      get: {
        operationId: "getUser",
        summary: "Get a user by ID",
        tags: ["Users"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        responses: {
          "200": { description: "User found" },
          "404": { description: "User not found" },
        },
      },
      put: {
        operationId: "updateUser",
        summary: "Update a user",
        tags: ["Users"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/UserInput" },
            },
          },
        },
        responses: {
          "200": { description: "User updated" },
        },
      },
      delete: {
        operationId: "deleteUser",
        summary: "Delete a user",
        tags: ["Users"],
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "integer" },
          },
        ],
        responses: {
          "204": { description: "User deleted" },
        },
      },
    },
    "/comments": {
      get: {
        operationId: "listComments",
        summary: "List comments",
        description: "List all comments with optional post filter.",
        tags: ["Comments"],
        parameters: [
          {
            name: "post_id",
            in: "query",
            schema: { type: "integer" },
          },
        ],
        responses: {
          "200": { description: "List of comments" },
        },
      },
      post: {
        operationId: "createComment",
        summary: "Create a comment",
        tags: ["Comments"],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/CommentInput" },
            },
          },
        },
        responses: {
          "201": { description: "Comment created" },
        },
      },
    },
    "/auth/login": {
      post: {
        operationId: "login",
        summary: "User login",
        description: "Authenticate a user and return a bearer token.",
        tags: ["Authentication"],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: {
                type: "object",
                properties: {
                  username: { type: "string" },
                  password: { type: "string", format: "password" },
                },
                required: ["username", "password"],
              },
            },
          },
        },
        responses: {
          "200": { description: "Login successful" },
          "401": { description: "Invalid credentials" },
        },
      },
    },
    "/auth/logout": {
      post: {
        operationId: "logout",
        summary: "User logout",
        tags: ["Authentication"],
        responses: {
          "200": { description: "Logout successful" },
        },
      },
    },
  },
  components: {
    schemas: {
      Post: {
        type: "object",
        properties: {
          id: { type: "integer" },
          title: { type: "string" },
          slug: { type: "string" },
          content: { type: "string" },
          author_id: { type: "integer" },
          published_at: { type: "string", format: "date-time" },
          tags: { type: "array", items: { type: "string" } },
        },
        required: ["id", "title", "slug", "content", "author_id"],
      },
      PostInput: {
        type: "object",
        properties: {
          title: { type: "string", minLength: 1, maxLength: 200 },
          content: { type: "string", minLength: 1 },
          author_id: { type: "integer" },
          tags: { type: "array", items: { type: "string" } },
        },
        required: ["title", "content", "author_id"],
      },
      User: {
        type: "object",
        properties: {
          id: { type: "integer" },
          username: { type: "string" },
          email: { type: "string", format: "email" },
          role: { type: "string", enum: ["admin", "editor", "reader"] },
          is_active: { type: "boolean" },
          created_at: { type: "string", format: "date-time" },
        },
        required: ["id", "username", "email", "role"],
      },
      UserInput: {
        type: "object",
        properties: {
          username: { type: "string", minLength: 3, maxLength: 50 },
          email: { type: "string", format: "email" },
          password: { type: "string", minLength: 8 },
          role: { type: "string", enum: ["admin", "editor", "reader"] },
        },
        required: ["username", "email", "password"],
      },
      CommentInput: {
        type: "object",
        properties: {
          post_id: { type: "integer" },
          author_id: { type: "integer" },
          body: { type: "string", minLength: 1 },
        },
        required: ["post_id", "author_id", "body"],
      },
    },
  },
};

export const mockResponseBodies: Record<string, unknown> = {
  listPosts: {
    results: [
      {
        id: 1,
        title: "Getting Started with Umbral",
        slug: "getting-started-with-umbral",
        content:
          "Umbral is a Django-inspired web framework in Rust. It gives you migrations, CRUD, admin, and REST APIs with compile-time guarantees.",
        author_id: 1,
        published_at: "2026-05-15T10:00:00Z",
        tags: ["rust", "tutorial"],
      },
      {
        id: 2,
        title: "Understanding the Plugin System",
        slug: "understanding-the-plugin-system",
        content:
          "Every feature in Umbral is a plugin. Auth, sessions, admin, tasks, and REST are all plugins. Structurally they are identical to a third-party one.",
        author_id: 2,
        published_at: "2026-05-20T14:30:00Z",
        tags: ["architecture", "plugins"],
      },
    ],
    count: 2,
    next: null,
    previous: null,
  },
  getPost: {
    id: 1,
    title: "Getting Started with Umbral",
    slug: "getting-started-with-umbral",
    content:
      "Umbral is a Django-inspired web framework in Rust. It gives you migrations, CRUD, admin, and REST APIs with compile-time guarantees.",
    author_id: 1,
    published_at: "2026-05-15T10:00:00Z",
    tags: ["rust", "tutorial"],
  },
  createPost: {
    id: 3,
    title: "New Post Title",
    slug: "new-post-title",
    content: "This is the content of the new post.",
    author_id: 1,
    published_at: "2026-06-03T08:00:00Z",
    tags: [],
  },
  updatePost: {
    id: 1,
    title: "Updated Post Title",
    slug: "getting-started-with-umbral",
    content: "Updated content goes here.",
    author_id: 1,
    published_at: "2026-05-15T10:00:00Z",
    tags: ["rust", "tutorial", "updated"],
  },
  listUsers: {
    results: [
      {
        id: 1,
        username: "dalmas",
        email: "dalmas@example.com",
        role: "admin",
        is_active: true,
        created_at: "2026-01-01T00:00:00Z",
      },
      {
        id: 2,
        username: "alice",
        email: "alice@example.com",
        role: "editor",
        is_active: true,
        created_at: "2026-02-15T00:00:00Z",
      },
      {
        id: 3,
        username: "bob",
        email: "bob@example.com",
        role: "reader",
        is_active: false,
        created_at: "2026-03-10T00:00:00Z",
      },
    ],
    count: 3,
    next: null,
    previous: null,
  },
  getUser: {
    id: 1,
    username: "dalmas",
    email: "dalmas@example.com",
    role: "admin",
    is_active: true,
    created_at: "2026-01-01T00:00:00Z",
  },
  createUser: {
    id: 4,
    username: "charlie",
    email: "charlie@example.com",
    role: "reader",
    is_active: true,
    created_at: "2026-06-03T08:00:00Z",
  },
  login: {
    token: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.mock-signature",
    user: {
      id: 1,
      username: "dalmas",
      email: "dalmas@example.com",
      role: "admin",
    },
  },
  logout: {
    message: "Successfully logged out.",
  },
  listComments: {
    results: [
      {
        id: 1,
        post_id: 1,
        author_id: 2,
        body: "Great introduction! Looking forward to more tutorials.",
        created_at: "2026-05-16T09:00:00Z",
      },
      {
        id: 2,
        post_id: 1,
        author_id: 3,
        body: "The plugin architecture is really elegant.",
        created_at: "2026-05-17T11:30:00Z",
      },
    ],
    count: 2,
    next: null,
    previous: null,
  },
  createComment: {
    id: 3,
    post_id: 1,
    author_id: 1,
    body: "Thanks for the feedback everyone!",
    created_at: "2026-06-03T08:00:00Z",
  },
};

export function getMockResponse(
  operationId: string,
  _status: number,
): { body: string; headers: Record<string, string> } | null {
  const body = mockResponseBodies[operationId];
  if (!body) return null;
  return {
    body: JSON.stringify(body, null, 2),
    headers: {
      "content-type": "application/json",
      "x-request-id": `req-${Math.random().toString(36).slice(2, 10)}`,
    },
  };
}
