---
$type: lifecycles/flow/v1
name: completion-install
command: deadband completion install
steps:
- id: install
  one_of:
  - {transition: completion#install-new}
  - {transition: completion#install-over}
crash: []
---
`install-new` writes the file where none exists, and `install-over` writes over
it. crumbkit's completion module decides how the file is written.
