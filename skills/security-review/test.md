# Tests for security-review

## checks
- CHECK frontmatter.name == "security-review"
- CHECK frontmatter.description contains "security checklist"
- CHECK manifest.name == "security-review"
- CHECK manifest.permissions.network == false
- CHECK manifest.permissions.file_write == false
- CHECK manifest.permissions.exec == false
- CHECK manifest.permissions.allowed_tools contains grep
- CHECK instructions contains "BLOCKER"
- CHECK instructions contains "API keys"
- CHECK instructions not contains "rm -rf"
