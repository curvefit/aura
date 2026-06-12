# Naming

Aura uses profile-specific extensions while the format is experimental:

```text
.aura.cold   canonical cold archive profile
.aura.warm   resolved fixed-integer profile
.aura.group  grouped hot experiment
.aura.hot    ultra hot replay cache profile
```

Compressed files can append the compression suffix:

```text
.aura.cold.zst
.aura.warm.zst
.aura.group.zst
.aura.hot.zst
```

`AUR0`, `AUR1`, `AUR2`, and `AUR3` are prototype magic values for cold, warm,
grouped hot, and ultra hot respectively. The numeric suffix maps to the profile,
not to the global format version.
