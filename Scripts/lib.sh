#!/usr/bin/env bash
set -euo pipefail

# Color output functions
color_red() { printf "\033[31m%s\033[0m\n" "$1"; }
color_green() { printf "\033[32m%s\033[0m\n" "$1"; }
color_yellow() { printf "\033[33m%s\033[0m\n" "$1"; }
color_blue() { printf "\033[34m%s\033[0m\n" "$1"; }
color_cyan() { printf "\033[36m%s\033[0m\n" "$1"; }
color_gray() { printf "\033[90m%s\033[0m\n" "$1"; }

# Logging functions with emoji
log_info() { echo "$(color_blue "ℹ️  $1")"; }
log_success() { echo "$(color_green "✅ $1")"; }
log_warning() { echo "$(color_yellow "⚠️  $1")"; }
log_error() { echo "$(color_red "❌ $1")"; }
log_debug() { 
    if [[ "${DEBUG:-0}" == "1" ]]; then
        echo "$(color_gray "🔍 $1")"
    fi
}

# Check if command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Ensure we're in the project root
ensure_project_root() {
    if [[ ! -f "project.yml" && ! -d "DevCanopy.xcodeproj" ]]; then
        log_error "Not in DevCanopy project root directory"
        exit 1
    fi
}

# Check for clean git working tree
ensure_clean_working_tree() {
    if ! git diff --quiet || ! git diff --staged --quiet; then
        log_error "Working tree is not clean. Please commit or stash changes."
        exit 1
    fi
}

# Get current git branch
get_current_branch() {
    git rev-parse --abbrev-ref HEAD
}

# Check if on main branch
ensure_main_branch() {
    local current_branch=$(get_current_branch)
    if [[ "$current_branch" != "main" ]]; then
        log_error "Not on main branch (current: $current_branch)"
        exit 1
    fi
}

# Get build number from git
get_build_number_from_git() {
    local count=$(git rev-list --count HEAD 2>/dev/null || echo "1")
    echo "$count"
}

# Parse version components
parse_version() {
    local version=$1
    echo "$version" | sed 's/^v//'
}

# Increment version
increment_version() {
    local version=$1
    local component=$2  # major, minor, or patch
    
    local major minor patch
    IFS='.' read -r major minor patch <<< "$(parse_version "$version")"
    
    case $component in
        major)
            ((major++))
            minor=0
            patch=0
            ;;
        minor)
            ((minor++))
            patch=0
            ;;
        patch)
            ((patch++))
            ;;
        *)
            log_error "Invalid version component: $component"
            exit 1
            ;;
    esac
    
    echo "${major}.${minor}.${patch}"
}

# Create app bundle directory structure
create_app_bundle() {
    local bundle_path=$1
    local executable_name=$2
    
    mkdir -p "$bundle_path/Contents/MacOS"
    mkdir -p "$bundle_path/Contents/Resources"
}

# Generate Info.plist from template
generate_info_plist() {
    local template_path=$1
    local output_path=$2
    local version=$3
    local build_number=$4
    
    sed -e "s/\${VERSION}/$version/g" \
        -e "s/\${BUILD_NUMBER}/$build_number/g" \
        "$template_path" > "$output_path"
}

# Wait for keypress
wait_for_keypress() {
    local prompt="${1:-Press any key to continue...}"
    read -n 1 -s -r -p "$prompt"
    echo
}

# Show spinner while command runs
run_with_spinner() {
    local command="$1"
    local message="${2:-Working...}"
    
    (
        eval "$command"
    ) &
    
    local pid=$!
    local spin='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
    local charwidth=3
    
    echo -n "$message "
    while kill -0 $pid 2>/dev/null; do
        local temp=${spin#?}
        printf "\b%s" "${spin:0:1}"
        spin=$temp${spin%"$temp"}
        sleep 0.1
    done
    
    wait $pid
    local exit_code=$?
    
    printf "\b"
    if [ $exit_code -eq 0 ]; then
        echo "$(color_green "✓")"
    else
        echo "$(color_red "✗")"
    fi
    
    return $exit_code
}

# Export functions for use in other scripts
export -f color_red color_green color_yellow color_blue color_cyan color_gray
export -f log_info log_success log_warning log_error log_debug
export -f command_exists ensure_project_root ensure_clean_working_tree
export -f get_current_branch ensure_main_branch get_build_number_from_git
export -f parse_version increment_version create_app_bundle generate_info_plist
export -f wait_for_keypress run_with_spinner