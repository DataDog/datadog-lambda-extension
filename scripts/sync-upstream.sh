#!/bin/bash

# Sync Upstream Script
# This script syncs your private fork with the upstream public repository

set -e  # Exit on any error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
UPSTREAM_REPO="DataDog/datadog-lambda-extension"
UPSTREAM_BRANCH="main"
TARGET_BRANCH="main"
UPSTREAM_URL="https://github.com/${UPSTREAM_REPO}.git"

# Function to print colored output
print_status() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Function to show usage
show_usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -h, --help          Show this help message"
    echo "  -f, --force         Force sync even if no new commits"
    echo "  -d, --dry-run       Show what would be synced without making changes"
    echo "  -v, --verbose       Show detailed output"
    echo "  --no-push           Don't push changes to origin (just merge locally)"
    echo ""
    echo "Examples:"
    echo "  $0                  # Normal sync"
    echo "  $0 --dry-run        # See what would be synced"
    echo "  $0 --force          # Force sync even if no new commits"
    echo "  $0 --no-push        # Merge locally but don't push"
}

# Parse command line arguments
DRY_RUN=false
FORCE_SYNC=false
VERBOSE=false
NO_PUSH=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -h|--help)
            show_usage
            exit 0
            ;;
        -f|--force)
            FORCE_SYNC=true
            shift
            ;;
        -d|--dry-run)
            DRY_RUN=true
            shift
            ;;
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        --no-push)
            NO_PUSH=true
            shift
            ;;
        *)
            print_error "Unknown option: $1"
            show_usage
            exit 1
            ;;
    esac
done

# Check if we're in a git repository
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    print_error "Not in a git repository!"
    exit 1
fi

# Check if we're on the target branch
CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" != "$TARGET_BRANCH" ]; then
    print_warning "Not on $TARGET_BRANCH branch (currently on $CURRENT_BRANCH)"
    read -p "Do you want to switch to $TARGET_BRANCH? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_status "Switching to $TARGET_BRANCH branch..."
        git checkout "$TARGET_BRANCH"
    else
        print_error "Aborting sync. Please switch to $TARGET_BRANCH branch first."
        exit 1
    fi
fi

# Check for uncommitted changes
if ! git diff-index --quiet HEAD --; then
    print_error "You have uncommitted changes. Please commit or stash them first."
    git status --short
    exit 1
fi

print_status "Starting upstream sync..."
print_status "Upstream: $UPSTREAM_REPO"
print_status "Branch: $TARGET_BRANCH"

# Add upstream remote if it doesn't exist
if ! git remote get-url upstream > /dev/null 2>&1; then
    print_status "Adding upstream remote..."
    git remote add upstream "$UPSTREAM_URL"
else
    # Update upstream URL in case it changed
    git remote set-url upstream "$UPSTREAM_URL"
fi

# Fetch latest changes from upstream
print_status "Fetching latest changes from upstream..."
git fetch upstream "$UPSTREAM_BRANCH"

# Get current and upstream commit hashes
CURRENT_COMMIT=$(git rev-parse HEAD)
UPSTREAM_COMMIT=$(git rev-parse "upstream/$UPSTREAM_BRANCH")

print_status "Current commit:  $CURRENT_COMMIT"
print_status "Upstream commit: $UPSTREAM_COMMIT"

# Check if there are new commits
if [ "$UPSTREAM_COMMIT" = "$CURRENT_COMMIT" ] && [ "$FORCE_SYNC" = false ]; then
    print_success "Already up to date with upstream!"
    exit 0
fi

# Get commit details
if [ "$UPSTREAM_COMMIT" != "$CURRENT_COMMIT" ]; then
    COMMIT_COUNT=$(git rev-list --count "$CURRENT_COMMIT".."upstream/$UPSTREAM_BRANCH")
    print_status "Found $COMMIT_COUNT new commit(s) to sync"
    
    if [ "$VERBOSE" = true ] || [ "$DRY_RUN" = true ]; then
        echo ""
        print_status "New commits:"
        git log --oneline "$CURRENT_COMMIT".."upstream/$UPSTREAM_BRANCH"
        echo ""
    fi
else
    print_status "Force sync requested (no new commits)"
fi

# Dry run mode
if [ "$DRY_RUN" = true ]; then
    print_warning "DRY RUN MODE - No changes will be made"
    print_status "Would merge $UPSTREAM_COMMIT into $CURRENT_COMMIT"
    if [ "$NO_PUSH" = false ]; then
        print_status "Would push changes to origin/$TARGET_BRANCH"
    fi
    exit 0
fi

# Perform the merge
print_status "Merging upstream changes..."
if git merge "upstream/$UPSTREAM_BRANCH" --no-edit; then
    print_success "Successfully merged upstream changes"
else
    print_error "Merge failed due to conflicts!"
    print_error "Please resolve conflicts manually and then run:"
    print_error "  git add ."
    print_error "  git commit"
    if [ "$NO_PUSH" = false ]; then
        print_error "  git push origin $TARGET_BRANCH"
    fi
    exit 1
fi

# Push changes to origin (unless --no-push is specified)
if [ "$NO_PUSH" = false ]; then
    print_status "Pushing changes to origin..."
    if git push origin "$TARGET_BRANCH"; then
        print_success "Successfully pushed changes to origin"
    else
        print_error "Failed to push changes to origin"
        exit 1
    fi
else
    print_warning "Skipping push to origin (--no-push specified)"
fi

# Final summary
echo ""
print_success "Upstream sync completed successfully!"
if [ "$UPSTREAM_COMMIT" != "$CURRENT_COMMIT" ]; then
    print_status "Synced $COMMIT_COUNT commit(s) from upstream"
fi
print_status "Current commit: $(git rev-parse HEAD)"
print_status "Upstream commit: $UPSTREAM_COMMIT"

# Show recent commits
echo ""
print_status "Recent commits:"
git log --oneline -5
