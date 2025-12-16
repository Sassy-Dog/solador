# DevCanopy

A native macOS dashboard for monitoring your development infrastructure at a glance.

![DevCanopy Dashboard](docs/images/dashboard.png)

## Features

- **Git Repository Monitoring** - Track local repositories' sync status with remotes
- **GitHub Integration** - Monitor workflow runs and CI/CD pipelines
- **Vercel Deployments** - See deployment status across all projects
- **Neon Databases** - Track database branch status and compute state
- **Cloudflare Resources** - Monitor Workers and Pages deployments

## Quick Start

1. Clone the repository:
```bash
git clone https://github.com/sassydog/devcanopy.git
cd devcanopy
```

2. Build and run:
```bash
./dev run
```

## Development

DevCanopy uses a comprehensive development workflow inspired by best practices:

### Common Commands

- `./dev` - Build and run in debug mode
- `./dev run --release` - Run release build  
- `./dev test` - Run all tests
- `./dev clean` - Clean build artifacts
- `./dev xcode` - Open in Xcode
- `./dev publish --bump patch` - Create a new release
- `./prd` - Build production version

### Requirements

- macOS 14.0 (Sonoma) or later
- Xcode 15.0 or later
- XcodeGen (`brew install xcodegen`)

### Project Structure

```
DevCanopy/
├── dev                    # Development script
├── Scripts/               # Build and utility scripts
├── DevCanopy/            # Source code
│   ├── App/             # App lifecycle
│   ├── Models/          # SwiftData models
│   ├── Services/        # Service integrations
│   ├── Views/           # SwiftUI views
│   └── Resources/       # Assets and config
└── DevCanopyTests/       # Unit tests
```

## Configuration

### Service Authentication

- **GitHub & Vercel**: OAuth 2.0 with PKCE flow
- **Neon & Cloudflare**: API token authentication

All credentials are securely stored in the macOS Keychain.

### Terminal Support

DevCanopy can open repositories in your preferred terminal:
- Terminal.app
- iTerm2
- Warp
- Ghostty

## Building for Release

1. Ensure you're on the main branch with a clean working tree
2. Run `./dev publish --bump patch` (or `minor`/`major`)
3. The script will:
   - Run tests
   - Update version number
   - Build release version
   - Create and push git tag

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- Built with Swift and SwiftUI
- Uses FSEvents for efficient file system monitoring
- Integrates with GitHub, Vercel, Neon, and Cloudflare APIs