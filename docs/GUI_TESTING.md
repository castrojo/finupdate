# GUI Testing for Finupdate

## Test Framework

Finupdate uses [behave](https://behave.readthedocs.io/) with [dogtail](https://en.wix.com/en/dogtail/) for GUI automation via AT-SPI (Assistive Technology Service Provider Interface).

Tests are located in `tests/smoke/features/finupdate.feature` and cover:
- Application launch and window detection
- Menu navigation and shortcuts
- Developer mode simulation scenarios (Success, AlreadyUpToDate, Failure)
- Clean shutdown

## Running Tests

```bash
# Run all smoke tests
cd tests/smoke
behave features/finupdate.feature

# Run specific test tag (e.g., @launch)
behave features/finupdate.feature --tags=@launch

# Run with verbose output
behave features/finupdate.feature --no-capture
```

## Requirements

### System Dependencies

The test framework requires:
- `gnome-shell` with AT-SPI support
- `at-spi2-core` (usually installed by default)
- **Missing**: `gnome-ponytail-daemon` and `python3-gnome-ponytail-daemon`

The ponytail daemon is needed for robust window ID detection and is currently **not included** in the Dakota image.

### Python Dependencies

- `behave` — BDD test runner
- `dogtail` — GUI automation via AT-SPI
- `qecore` — Project Blue Fin test harness

Install with:
```bash
pip install behave dogtail qecore
```

## Current Limitations

### Ponytail Daemon Missing

Without `gnome-ponytail-daemon`, dogtail cannot reliably:
- Detect window IDs
- Verify window state
- Handle keyring dialogs

**Error message**:
```
Error in ponytail initiation might be cause by several reasons:
  1) Packages '['gnome-ponytail-daemon', 'python3-gnome-ponytail-daemon']' are not installed.
  2) If installed, the gnome-ponytail-daemon process might not be running.
  3) You are on the system that does not have GNOME Shell Introspection.
```

### Workarounds

1. **Manual Testing**: Run `flatpak run org.projectbluefin.Finupdate.Devel` and interact with the GUI directly
2. **Log-based Verification**: Check application logs for structured output instead of GUI automation
3. **Developer Mode**: Test update scenarios via dev mode without touching the real system:
   ```bash
   # Settings are in: ~/.var/app/org.projectbluefin.Finupdate.Devel/config/finupdate/settings.json
   # Set: { "dev_mode": true, "sim_scenario": "Success" }
   ```

## Test Structure

### Feature File: `finupdate.feature`

Defines BDD scenarios with steps executed against the running application:

```gherkin
@launch
Scenario: Application window appears with the main controls
  * Application "finupdate" is running
  * Item "Check for Updates" "push button" is "showing" in "finupdate"
```

### Step Definitions: `steps/steps.py`

Custom steps implemented in Python:
- `Wait until window "Finupdate" appears in "finupdate"` — Flexible window detection (handles GTK4 role variations)
- `Application "finupdate" is in developer mode with scenario "Success"` — Pre-configure dev mode before launch
- `Wait until "{text}" appears in "finupdate" within {seconds:d} seconds` — Bounded text search with timeout

### Environment: `environment.py`

Test harness setup using `qecore.TestSandbox`:
- Configures Flatpak launching
- Manages AT-SPI accessibility
- Captures journal slices and artifacts on failure

## Upstream Issue

A request has been filed on projectbluefin/dakota to include `gnome-ponytail-daemon` and `python3-gnome-ponytail-daemon` in the base image. This will enable full GUI test automation without workarounds.

## Future Improvements

1. **Include ponytail daemon** in Dakota base image
2. **Add integration tests** for actual update flows (with real bootc)
3. **Snapshot AT-SPI tree** on test failure for debugging
4. **CI/CD integration** using qecore's headless session support

## References

- [dogtail Documentation](https://en.wix.com/en/dogtail/)
- [AT-SPI Overview](https://wiki.gnome.org/Accessibility/ATSPI2)
- [GTK Accessibility](https://developer.gnome.org/gtk4/stable/gtk-running.html#running-and-debugging-GTK-Applications)
- [qecore Testing Framework](https://github.com/projectbluefin/testing-lab)
