#!/usr/bin/env sh
set -eu

# Uninstalls Crab that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Crab variants in /Applications
        remaining=$(ls -d /Applications/Crab*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Crab variants in ~/.local
        remaining=$(ls -d "$HOME/.local/crab"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Crab preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/crab"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${ZED_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Crab has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="me.farshed.Crab"
        db_suffix="stable"
        ;;
      dev)
        appid="me.farshed.Crab-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="me.farshed.Crab"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/crab$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/crab"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/crab/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/crab/crab-$db_suffix.sock"

    # Remove the entire Crab directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/crab"
        prompt_remove_preferences
    fi

    rm -rf "$HOME/.crab_server"
}

macos() {
    app="Crab.app"
    db_suffix="stable"
    app_id="me.farshed.Crab"
    case "$channel" in
      dev)
        app="Crab Dev.app"
        db_suffix="dev"
        app_id="me.farshed.Crab-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/crab"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Crab/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Crab directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Crab"
        rm -rf "$HOME/Library/Logs/Crab"

        prompt_remove_preferences
    fi

    rm -rf "$HOME/.crab_server"
}

main "$@"
