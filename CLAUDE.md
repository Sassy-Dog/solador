# DevCanopy - AI Assistant Instructions

This file provides context for Claude Code when working with the DevCanopy codebase.

## Project Overview

DevCanopy is a native macOS dashboard application that monitors development infrastructure:
- Local git repositories (FSEvents-based monitoring)
- GitHub workflows and CI/CD status
- Vercel deployments
- Neon databases
- Cloudflare Workers/Pages

## Development Workflow

### Quick Commands
- `./dev` - Build and run (debug mode)
- `./dev run --release` - Run release build
- `./dev test` - Run all tests
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish --bump patch` - Publish new version
- `./prd` - Production build (alias for `./dev build --release`)

### Project Structure
```
DevCanopy/
├── dev                     # Development script (entry point)
├── prd                     # Production build script
├── Scripts/                # Build and development scripts
│   ├── lib.sh             # Common functions
│   ├── config.sh          # App configuration
│   └── *.sh               # Implementation scripts
├── project.yml            # XcodeGen configuration
├── DevCanopy/             # Source code
│   ├── App/              # App lifecycle
│   ├── Models/           # SwiftData models
│   ├── Services/         # API integrations
│   ├── Views/            # SwiftUI views
│   └── Resources/        # Info.plist, entitlements
└── DevCanopyTests/        # Unit tests
```

## Technology Stack
- **Language**: Swift 5.9+
- **UI Framework**: SwiftUI
- **Data Persistence**: SwiftData
- **Credential Storage**: macOS Keychain
- **File Monitoring**: FSEvents
- **OAuth**: ASWebAuthenticationSession with PKCE
- **Project Generation**: XcodeGen

## Key Implementation Notes

### Git Monitoring
- Uses FSEvents to watch repository directories
- Parses git state without modifying files
- Compares local HEAD to remote tracking branch
- Updates status every 30s/1m/5m based on settings

### Service Authentication
- **GitHub/Vercel**: OAuth 2.0 with PKCE (no client secret)
- **Neon/Cloudflare**: API token authentication
- All credentials stored in macOS Keychain

### UI Design
- Dark mode optimized (primary mode)
- Glanceable dashboard view
- Status indicated by color: green (good), orange (warning), red (error)
- Click actions open terminal/browser in context
- Designed for persistent display on second monitor

### SwiftData Models
- `TrackedRepository`: Local git repos with status
- `ServiceConnection`: OAuth/API connections
- `AppSettings`: User preferences

## Testing

Run tests with:
```bash
./dev test
```

Tests cover:
- Repository status calculations
- Service connection states
- Data model persistence
- UI component behavior

## Building for Distribution

1. Ensure clean working tree on main branch
2. Run `./dev publish --bump patch` (or minor/major)
3. Script will:
   - Run tests
   - Update version
   - Build release
   - Create git tag
   - Push to GitHub

## Common Tasks

### Adding a New Service Integration
1. Add to `ServiceType` enum in `ServiceConnection.swift`
2. Create service folder in `DevCanopy/Services/`
3. Implement OAuth flow or API token auth
4. Add UI in `ServicesView.swift`

### Updating Git Monitoring
- Main logic in `Services/GitMonitor/`
- FSEvents setup in `GitMonitor.swift`
- Status parsing in `GitStatusParser.swift`

### Modifying Build Scripts
- Edit scripts in `Scripts/` directory
- Common functions in `lib.sh`
- Configuration in `config.sh`

## Debugging

- Enable debug logging: `DEBUG=1 ./dev run`
- Check Console.app for app logs
- SwiftUI previews available for most views

## Important Conventions

1. Use SwiftUI's environment for dependency injection
2. Keep views small and focused
3. Business logic in Services, not Views
4. Use SwiftData's @Query for reactive updates
5. Handle errors gracefully with user-friendly messages

## Security Considerations

- Never log credentials or tokens
- Use Keychain for all sensitive data
- Request minimal OAuth scopes
- Validate SSL certificates
- No telemetry or analytics by default