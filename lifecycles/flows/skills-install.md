---
$type: lifecycles/flow/v1
name: skills-install
command: deadband skills install
steps:
- id: install
  one_of:
  - {transition: skill#install-new}
  - {transition: skill#install-over}
crash: []
---
`install-new` writes a copy where none exists, and `install-over` writes over
one. crumbkit_skills decides how the copy is written.
