# Contributing to Ion

Thanks for your interest in Ion.

## Before you start

- Read [docs/vision.md](docs/vision.md). Ion is deliberately minimal, so
  features that don't fit the vision (for example a built-in AI chat) won't be
  accepted.
- For anything larger than a small fix, open an issue first so we can agree on
  the approach.

## Workflow

1. Set up your environment with [docs/development.md](docs/development.md).
2. Create a branch from `main`.
3. Keep changes focused: one topic per pull request.
4. Before pushing, run:

   ```bash
   cargo fmt --all
   cargo lint
   cargo t
   ```

5. Open a pull request using the template.

## Guidelines

- Follow the layering rules in [docs/architecture.md](docs/architecture.md):
  `text`, `project` and `fuzzy` must not depend on GPUI.
- Respect the [performance budgets](docs/performance.md): no polling, no idle
  timers, no extra thread pools.
- Add tests for logic changes.
- Write commit messages in the imperative mood ("Add file watcher", not "Added file watcher").

## License

By contributing, you agree that your contributions are dual licensed under
MIT OR Apache-2.0, as described in the [README](README.md#license).
