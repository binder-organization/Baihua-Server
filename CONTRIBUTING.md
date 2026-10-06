# Baihua Contributor Guide

Thank you for your interest in and support of the Baihua project! Baihua is a tool designed to enhance developer collaboration efficiency. We warmly welcome your contributions, whether it's reporting issues, suggesting improvements, or directly participating in code development. Here are some guidelines to help you better contribute to the Baihua project.

## Baihua Design Philosophy: Core-First, API-Driven, Transparent & Explainable

In today's development work, we often need to switch frequently between multiple tools and platforms: for example, operating Git in the terminal, checking project Issues in the browser, managing CI/CD in another panel, and tracking progress using spreadsheets or separate tools... This fragmented workflow not only reduces efficiency but also interrupts the developer's most precious state of "flow".

Baihua's design philosophy aims to solve this problem, pursuing the following core values:

1.  **Core-First**: We prioritize building a stable, powerful, and scalable backend service, ensuring all business logic and data management meet production-grade standards.
2.  **API-Driven**: All functionalities are exposed through clear APIs. This provides ultimate flexibility for integration and any form of future client development.
3.  **Transparency & Explainability**: While we are committed to automating all repetitive tasks, we firmly believe that any automated operation must be fully explainable and traceable. We ensure that "what happened" and "why it happened" are completely transparent to developers.

Baihua's design philosophy always revolves around one goal: to minimize developers' context switching and cognitive load. Through a refined backend core, open integration capabilities, and transparent operational mechanisms, it allows engineers to refocus on creation itself.

## How to Contribute

### 1. Report Issues

If you encounter any problems while using Baihua, feel free to submit a new issue in the project's [Issue](https://github.com/Binder-organize/Baihua-Server/issues) tracker.

*   **Issue Description**: Describe the problem you encountered in detail.
*   **Reproduction Steps**: Provide steps to reproduce the issue.
*   **Expected Result**: Describe the result you expected.
*   **Actual Result**: Describe the result you actually got.
*   **Environment Information**: Include information such as the operating system, Baihua version, etc.

### 2. Suggest Features

If you think Baihua is missing certain features, or if you have suggestions for improving existing features, you are also welcome to submit a new [Issue](https://github.com/Binder-organize/Baihua-Server/issues).

Before submitting, please make sure:
*   You have searched and **found no** similar existing issues.
*   You have searched the documentation and **found no** related content.
*   You have tried using the **latest version**, and the problem persists.
*   You have **not** replaced or modified any program files.

In your suggestion, please be sure to include the following information:
*   **Feature Description**: Describe in detail the feature you wish to add or improve.
*   **Use Case**: Provide the practical development scenario where this feature would be used.
*   **Implementation Suggestions**: If you have specific suggestions for implementation, you can also propose them here.

### 3. Contribute Code

If you are interested in Baihua's code and wish to participate directly in development, you can follow these steps:

> [!IMPORTANT]
> You may have noticed that Baihua uses `gitflow` as its branching management strategy. Therefore, before starting development, please ensure you understand this strategy and how to use it.

1.  **Fork the Repository**: `Fork` the Baihua repository to your personal account on `GitHub`.
2.  **Clone the Repository**: Clone your forked repository to your local development environment.
3.  **Create a Branch**: Create a new branch for the feature you want to develop or the issue you want to fix.
4.  **Write Code**: Write and test your code on the branch.
5.  **Commit Code**: Commit your code to the new branch and push it to `GitHub`. Regarding commit messages, please refer to the [Commit Message Convention](#commit-message-convention).
6.  **Create a Pull Request**: Create a `Pull Request` on `GitHub` to merge your branch into Baihua's corresponding development branch.

When submitting code, please follow these conventions **in order**:

1.  **Code Style**: Run `cargo fmt` and `cargo clippy --locked -- -D warnings` to format and lint your code.
2.  **Integration Tests**: Run the full test suite with `python3 tests/run_tests.py`. See [Testing](#testing) below for details.
3.  **Documentation**: Update relevant documentation, including files in the `/docs/` directory, the `README.md` file in the root directory, etc. API documentation is published on the [Baihua Server Documentations](https://binder-organize.github.io/Baihua-Server-Docs). When adding or modifying API documentation, edit the corresponding Markdown files in the [Baihua-Server-Docs](https://github.com/Binder-organize/Baihua-Server-Docs) repository (under `docs/` for English, `i18n/zh-CN/` for Chinese).

> [!CAUTION]
> Please be sure to follow the above rules, otherwise your code may be rejected for merging.

## Testing

Baihua uses **Python pytest** for HTTP integration tests against a running server. Tests live in `tests/`.

### Prerequisites

- **Docker** (for PostgreSQL database via `docker compose`)
- **Python 3.10+** with `pip install -r tests/requirements.txt`

### Run Tests

The test runner handles the full lifecycle: build the Rust binary, start the database, launch the server, run tests, and clean up.

```bash
# Default mode (recommended): DB in Docker, server binary local
python3 tests/run_tests.py

# Full Docker mode (like CI): builds and runs everything in containers
python3 tests/run_tests.py --docker

# Local PostgreSQL mode: provide POSTGRES_HOST, POSTGRES_PORT,
# POSTGRES_USER, POSTGRES_PASSWORD, and a dedicated baihua_test_ database
python3 tests/run_tests.py --local
```

Pre-flight checks (`cargo fmt --check` and `clippy`) run automatically. To skip them:

```bash
python3 tests/run_tests.py --skip-checks
```

To keep the server running after tests (e.g. for manual testing):

```bash
python3 tests/run_tests.py --keep
```

The runner creates a unique Docker Compose project and database for each run.
Local mode requires an explicitly named test database and never selects an
existing application database automatically. Run a small subset with
`--smoke`, or repeat `--test` to select individual test paths or cases.

### CI Pipeline

Two GitHub Actions workflows run on every push/PR:

| Workflow | File | What it checks |
|----------|------|----------------|
| **check** | `.github/workflows/check.yml` | `cargo fmt --check`, `cargo check --locked`, `clippy` |
| **integration** | `.github/workflows/integration.yml` | Full Docker build, smoke test, pytest integration tests |

Both must pass before a pull request can be merged.

#### Commit Message Convention

The commit message format is as follows:
```
<type>: <subject>
<BLANK LINE>
<body>
<BLANK LINE>
<footer>
```

The commit `type` must be one of the following:
*   `build`: Changes that affect the build system or external dependencies
*   `ci`: Changes to CI configuration files or scripts
*   `docs`: Documentation only changes
*   `feat`: A new feature
*   `fix`: A bug fix
*   `pref`: A code change that improves performance
*   `refactor`: A code change that neither fixes a bug nor adds a feature
*   `style`: Changes that do not affect the meaning of the code (white-space, formatting, missing semi-colons, etc.)
*   `test`: Adding missing tests or correcting existing tests

**Subject**
A brief description of the change. Requirements:
*   Use the **imperative mood**, present tense.
*   Do **not** capitalize the first letter.
*   Do **not** add a period at the end.

**Body**
*   Similar to the subject, use the imperative mood, present tense.
*   Should include the motivation for the change and contrast it with previous behavior.

**Footer**
If the commit is intended to address an issue, reference the issue in the footer.

Start with a keyword like `Closes`, e.g.,
```
Closes #234
```
If multiple issues are addressed, separate them with commas, e.g.,
```
Closes #123, #245, #992
```
If the commit contains a revert operation, the type should start with `revert:`, and the body should include `This reverts commit [hash]`, where the hash is the commit being reverted.

## Contact Us

If you have any questions or suggestions, feel free to contact us via:

*   Gavin Zheng <gav.zheng@outlook.com>
*   GitHub Repository: [Binder-organize/Baihua-Server](https://github.com/Binder-organize/Baihua-Server)
