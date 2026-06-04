# Requirements

## Core Concepts

### Repositories

Repositories can be anywhere accesed by different accounts. For example, I could have multiple accounts for GitHub and with repos also in GitLab or Bitbucket

Examples
https://github.com/Sassy-Dog/brewslate.git (monorepo)

https://github.com/Sassy-Dog/velovate.git (hybrid)
https://github.com/Sassy-Dog/velovate-web.git (marketing site)



### Applications

Applications are the highest level for rolling up reporting. Applications have Components that are mapped to Repositories. Applications have list of environments. Branches in a Repository can be mapped to Environments.

BrewSlate
	Environments
		PRD
		DEV_PERSONAL

	List of all Repositories associated with BrewSlate
		brewslate - https://github.com/Sassy-Dog/brewslate.git
			main -> mapped to PRD
	 
	List of All Components that make up BrewSlate. Components get mapped to repos. Components also get mapped to infrastructure by environment
		Dashboard -> brewslate (repo) -> Vercel project 1 & Neon
		Menu -> brewslate -> Vercel 2 & Neon
		Marketing -> brewslate -> Vercel 3 & Neon
		Display -> brewslate -> Vercel 4 & Neon





Velovate


Dashboard shows applications and environments to watch with rolled up status

|Application     |Monitored Environments      |Status      |
| ---- | ---- | ---- |
|BrewSlate      |Production      |Green      |
|Velovate      |Production      |      |
|TaxCalc      |Production + Staging + QA|Orange      |

Application | Environment | Status
BrewSlate | Production | Green
Velovate | Production | Green
TaxCalc | Production + Staging + QA | Orange

Key things I want to know.
Production good
Key GitHub Workflow Good (CI, Release, Drift Check)
How far my local repos are behind remote. Potential conflicts?

Need a rule enginge. 
For example, when I look at the dashboard I want see a list of apps and the status based on the rules I have defined.

BrewSlate |Green
Velovate |Green

BrewSlate is Green when the last CI on main is green the last Release on main was green, and the last drift check on Main is green.

I want to know how far behind I am on main and I want to know how far i am behind or ahead of the branch i am working on.


