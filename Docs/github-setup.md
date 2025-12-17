# Setting up GitHub Integration

DevCanopy supports two methods of connecting to GitHub:

## Method 1: Personal Access Token (Recommended for Quick Setup)

1. Navigate to **Services** in the sidebar
2. Click **Connect Service** and select **GitHub**
3. Choose **Personal Access Token** tab
4. Click **Create a Personal Access Token on GitHub** to open GitHub settings
5. Create a new token with these scopes:
   - `repo` - Full control of private repositories
   - `workflow` - Update GitHub Action workflows
6. Copy the generated token (it will only be shown once!)
7. Paste it into DevCanopy and click **Connect**

## Method 2: OAuth (Coming Soon)

OAuth authentication is more secure but requires a GitHub OAuth App. This feature requires a backend service for token exchange and is not yet implemented.

## After Connecting

Once connected, DevCanopy will:
- Auto-detect GitHub repositories from your local git remotes
- Monitor GitHub Actions workflows
- Show workflow status in repository cards
- Allow you to configure which workflows to track

## Troubleshooting

### Invalid Token Error
- Ensure your token has the required scopes (`repo` and `workflow`)
- Check that the token hasn't expired
- Try generating a new token

### Workflows Not Showing
- Make sure the repository has a GitHub remote configured
- Check that the repository identifier is correctly set (format: `owner/repo`)
- Click **Refresh Workflows** in the repository settings

### Rate Limiting
GitHub has API rate limits:
- Authenticated requests: 5,000 per hour
- Unauthenticated requests: 60 per hour

DevCanopy shows rate limit status in the GitHub service card.