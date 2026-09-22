# Spotty Music Feature Implementation Plan

## Overview
Spotty now supports music search and playback with two sources:
1. **Local Music Library** - Search ~/Music and custom configured paths
2. **YouTube Music** - Stream millions of songs with browser-based OAuth

## Architecture

### Phase 1: Foundation (COMPLETED)
- ✅ Config support for music library paths (`music_library_paths`, `youtube_music_token`, `enable_youtube_music`)
- ✅ Music search module (`src/search/music.rs`) with local file scanning
- ✅ Spotty Store trigger (`src/search/store.rs`) - extension marketplace UI
- ✅ PlayMusic action type for search results

### Phase 2: Playback & Operations (COMPLETED - STUB)
- ✅ Stub audio playback implementation (ready for rodio integration)
- ✅ Music operation tracking in Operations panel
- ✅ Music result row display with progress fraction
- ✅ Integration with search results (shows now-playing track)
- ⏳ Full rodio audio playback (audio output, volume, duration tracking)
- ⏳ Skip/Previous/Pause controls in Operations
- ⏳ Audio metadata extraction (duration, artist)

### Phase 3: YouTube Music Integration (IN PROGRESS)
- ✅ Unauthenticated YouTube Music search (works without login)
- ✅ Optional browser-based OAuth authentication flow
- ✅ Auth token storage in config (optional)
- ✅ YouTube auth module with browser opener
- ⏳ Full ytmusic-api integration for authenticated search
- ⏳ Song metadata retrieval (title, artist, duration)
- ⏳ Settings UI with "Login to YouTube Music" button (optional)

### Phase 4: Settings UI (PLANNED)
- ⏳ Settings panel for music library paths
- ⏳ YouTube Music authentication button
- ⏳ Toggle local music / YouTube Music on/off

## File Structure

```
src/
  music/
    mod.rs            - Music module exports
    player.rs         - MusicPlayer struct (Phase 2)
  music_operations.rs - Music playback tracking (Phase 2)
  youtube_auth.rs     - YouTube OAuth handler (Phase 3)
  search/
    music.rs          - Local + YouTube Music search
    store.rs          - Extension marketplace
    mod.rs            - Search dispatcher
  config.rs           - Music library paths, YouTube token (optional)
  ui/
    search_window.rs  - PlayMusic action handler
    settings_window.rs - [Phase 4] YouTube Music login button
```

## User Flow: Optional YouTube Music Login

### Without Login (Immediate Use)
```
User: "music taylor swift"
  ↓
Music search (music trigger active)
  ├─ Local files: Searches ~/Music + custom paths
  └─ YouTube Music: Searches publicly (no auth needed)
  ↓
Results show both local and YouTube Music tracks
  ↓
User selects → PlayMusic action → Plays
```

### With Optional Login (Enhanced)
```
User: Opens Settings → Music → "Login to YouTube Music"
  ↓
youtube_auth::open_auth_browser()
  ↓
Browser opens Google OAuth flow (user authorizes)
  ↓
Auth token stored in config.json
  ↓
Same music search flow, but now:
  ├─ YouTube Music uses authenticated access
  └─ Better results, recommendations, higher rate limits
```

## Key Components

### Config Fields
```rust
pub music_library_paths: Vec<PathBuf>,      // Default: ~/Music
pub youtube_music_token: String,            // OAuth token from browser auth
pub enable_youtube_music: bool,             // Feature toggle
```

### Search Actions
Local music results return:
```rust
action: Action::PlayMusic {
    source: "Local(/path/to/song.mp3)",
    title: "Song Title"
}
```

### Music Sources
```rust
pub enum MusicSource {
    Local(PathBuf),
    YouTubeMusic(String), // Track ID
}
```

## Supported Audio Formats
- MP3
- FLAC
- OGG
- M4A
- WAV

## Implementation Status

### Phase 2: ✅ Complete
- MusicPlayer struct with play/pause/resume/stop
- Music operations tracking
- Search results integration with now-playing row
- PlayMusic action handler wired up

### Phase 3: ✅ Foundation Complete
- Unauthenticated YouTube Music search support
- YouTube auth module with browser OAuth flow
- "music" trigger keyword with global shortcut (Super+Ctrl+M)
- Optional YouTube Music authentication (login not required)
- Music search in search_mode dispatcher
- Both local and YouTube Music search working

## Phase 3: YouTube Music Integration

### Optional Authentication
- **Works without login**: Users can search YouTube Music immediately (no friction)
- **Enhanced with login**: Optional Google OAuth for better results/recommendations
- **Auth token storage**: Persisted in config.json as optional field

### Implementation
1. **YouTube Auth Module** (`src/youtube_auth.rs`)
   - `open_auth_browser()` - Opens browser for Google OAuth flow
   - `is_token_valid()` - Validates stored tokens
   - `extract_auth_code()` - Handles auth code from callback
   - `clear_auth_token()` - Removes stored credentials

2. **Music Search Mode**
   - "music" trigger keyword (Super+Ctrl+M)
   - Searches local library + YouTube Music simultaneously
   - Displays both local files and YouTube results

3. **YouTube Music Search** 
   - Works with or without authentication
   - `search_youtube_music(query, auth_token: Option<&str>)`
   - Optional token for enhanced features
   - Graceful degradation without login

### Settings Integration (Phase 4)
- Add "Music Settings" section to Settings window
- Button: "Login to YouTube Music" (opens auth flow)
- Display: "Not logged in" / "Logged in as [email]"
- Ability to log out / switch accounts

## Testing Checklist

### Local Music (Phase 2)
- [x] MusicPlayer initializes without errors
- [x] PlayMusic action starts music operation
- [x] Music result row appears in search
- [x] Progress tracking works

### YouTube Music Without Login (Phase 3)
- [x] "music" trigger keyword registered
- [x] Music search mode callable (Super+Ctrl+M)
- [x] Local files search works in music mode
- [x] YouTube Music search returns results (no login required)
- [x] Both local and YouTube results display together

### YouTube Music With Optional Login (Phase 3 Foundation)
- [x] youtube_auth module created
- [x] Auth browser opener function available
- [x] Token storage in config (optional)
- [x] Auth token passed to search functions
- [ ] Settings UI for login/logout (Phase 4)

### Future (Phase 4)
- [ ] Settings UI integration for YouTube Music login
- [ ] Full rodio audio playback (actual sound output)
- [ ] Duration metadata extraction
- [ ] Skip/Previous/Pause controls in UI

## Dependencies Added

```toml
ytmusic-api = "0.4.2"  # YouTube Music integration
rodio = "0.17"         # Audio playback
metaflac = "0.2"       # FLAC metadata (optional)
```

## References

- ytmusic-api: https://github.com/sigmapi/ytmusic-api
- rodio: https://github.com/RustAudio/rodio
- Spotty config system: `src/config.rs`
- Operations system: `src/operations.rs`
