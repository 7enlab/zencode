# Tests for tdd

## checks
- CHECK frontmatter.name == "tdd"
- CHECK frontmatter.description contains "Test-driven"
- CHECK manifest.name == "tdd"
- CHECK manifest.version == "1.0.0"
- CHECK manifest.permissions.exec == true
- CHECK manifest.permissions.file_write == true
- CHECK manifest.permissions.network == false
- CHECK manifest.permissions.allowed_tools contains bash
- CHECK manifest.permissions.allowed_tools contains read
- CHECK instructions contains "RED-GREEN-REFACTOR"
- CHECK instructions contains "failing test"
- CHECK instructions not contains "rm -rf"
