# Changelog
All notable changes to this project will be documented in this file.

## [0.3.2] - 2025-??-??
### Added
- Used BusRecoveryType::Serial to auto recover for some xum1541 failures, but only if the same serial number xum1541 is detected

## [0.3.1] - 2025-02-08
### Changed
- Moved to rs1541 0.3.1 (to pick up xum 0.3.1 serial number fix)

## [0.3.0] - 2025-01-31
### Added
- install.sh script and udev rules file for xum1541

### Changed
- Removed futures from Cargo.toml as unused
- Removed bindgen from Cargo.toml as no longer used (was required to build OpenCBM dependencies)
- Better readme and arg handling
- Improved logging
- Moved to rs1541 0.3.0 to support xum1541 0.3.0 traits

## [0.2.1] - 2025-01-27
### Added
- Many things

### Changed
- Many things

## [0.2.0] - 2025-01-23
### Added

### Changed
- Moved to rs1541 0.2 and xum1541 (replacing OpenCBM)

## [0.1.0] - 2025-01-18
### Added
- Initial release
