# Custom Storage Backend Still Mounts Local ServeDir

Category: Correctness, Security
Severity: Medium

## Finding

`media_with_storage` accepts an arbitrary storage backend, but still configures a local directory based on the URL mount and wires `ServeDir` for media routes. For non-filesystem backends, this can expose an unrelated local directory if one exists at that path.

## Evidence

- `plugins/umbral-storage/src/lib.rs:299-317` sets `dir: PathBuf::from(&mount)` for custom storage.
- `plugins/umbral-storage/src/lib.rs:597-667` mounts media serving through `ServeDir`.

## Risk

An app using S3 or another custom backend could accidentally serve files from a local `media` directory that was not intended to be public.

## Recommendation

Separate local file serving from custom storage:

- Do not mount `ServeDir` for non-filesystem backends unless a local directory is explicitly supplied.
- Route all custom backend media requests through the backend abstraction.
- Add a boot warning when the custom backend path also exists locally.

## Suggested Tests

- Custom backend with a local `media/secret.txt` does not serve that file unless explicitly configured.
- Filesystem backend still serves from the intended directory.

