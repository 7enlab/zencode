# Tests for token-diet

## checks
- CHECK frontmatter.name == "token-diet"
- CHECK frontmatter.description contains "Token-efficiency"
- CHECK manifest.name == "token-diet"
- CHECK manifest.permissions.network == false
- CHECK manifest.permissions.file_write == false
- CHECK manifest.permissions.exec == false
- CHECK instructions contains "fewest words"
- CHECK instructions contains "grep"
- CHECK instructions not contains "rm -rf"
