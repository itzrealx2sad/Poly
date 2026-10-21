use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use tokio::task::JoinSet;
use ytmusic::YtMusic;

use crate::youtube::{genres, radio, subscriptions, wire};
use crate::{
    Album, AlbumCatalogue, AlbumDetail, Artist, ArtistCatalogue, ArtistProfile, Feed, Genre,
    GenreDetail, HomeFeed, MediaKind, MusicApi, Playlist, PlaylistDetail, SUGGESTIONS, SavedArtist,
    Track, UserProfile, escape,
};

const PORTRAIT_LIMIT: usize = 24;
const EPISODES: &str = "SE";

pub struct YouTubeClient {
    api: Arc<YtMusic>,
    account: String,
    store: Arc<crate::local::Store>,
}

impl YouTubeClient {
    pub fn new(api: Arc<YtMusic>) -> Self {
        Self {
            api,
            account: String::new(),
            store: Arc::new(crate::local::Store::new(storage::Database::standard())),
        }
    }

    pub fn owned_by(mut self, account: impl Into<String>) -> Self {
        self.account = account.into();
        self
    }

    fn is_guest(&self) -> bool {
        self.account.is_empty()
    }

    async fn hydrate_durations(&self, tracks: &mut [ytmusic::Track]) {
        let mut tasks = JoinSet::new();
        for (index, track) in tracks.iter().enumerate() {
            if track.duration.is_some() {
                continue;
            }
            let Some(video_id) = track.video_id.clone() else {
                continue;
            };
            let api = self.api.clone();
            tasks.spawn(async move { (index, api.track_duration(&video_id).await) });
        }
        while let Some(result) = tasks.join_next().await {
            if let Ok((index, Some(duration))) = result {
                tracks[index].duration = Some(duration);
            }
        }
    }
}

#[async_trait]
impl MusicApi for YouTubeClient {
    fn share_url(&self, kind: MediaKind, id: &str) -> Option<String> {
        let id = escape::component(id);
        let url = match kind {
            MediaKind::Track => format!("https://music.youtube.com/watch?v={id}"),
            MediaKind::Album => format!("https://music.youtube.com/browse/{id}"),
            MediaKind::Artist => format!("https://music.youtube.com/channel/{id}"),
            MediaKind::Playlist => format!("https://music.youtube.com/playlist?list={id}"),
        };
        Some(url)
    }

    async fn profile(&self) -> Result<UserProfile> {
        Ok(wire::profile(self.api.profile().await?))
    }

    async fn artist(&self, artist_id: &str) -> Result<Artist> {
        let mut artist = self.api.artist(artist_id).await?;
        self.hydrate_durations(&mut artist.top_tracks).await;
        Ok(wire::artist(artist))
    }

    /// The whole discography, since the artist page `artist` reads holds only ten albums and
    /// ten singles.
    async fn artist_catalogue(&self, artist_id: &str, _known: &[Track]) -> Result<ArtistCatalogue> {
        let albums = self
            .api
            .discography(artist_id)
            .await
            .with_context(|| format!("cannot load the discography of artist {artist_id}"))?;
        Ok(ArtistCatalogue {
            albums: albums.into_iter().map(wire::album).collect(),
            ..ArtistCatalogue::default()
        })
    }

    async fn artist_profile(&self, artist_id: &str) -> Result<ArtistProfile> {
        Ok(wire::artist_profile(&self.api.artist(artist_id).await?))
    }

    async fn artist_images(&self, ids: Vec<String>) -> Result<HashMap<String, String>> {
        let mut tasks = JoinSet::new();
        for id in ids.into_iter().take(PORTRAIT_LIMIT) {
            let api = self.api.clone();
            tasks.spawn(async move {
                let artist = api.artist(&id).await.ok()?;
                let image = wire::cover_large(&artist.thumbnails)?;
                Some((id, image))
            });
        }
        let mut images = HashMap::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(Some((id, image))) = result {
                images.insert(id, image);
            }
        }
        Ok(images)
    }

    async fn saved_tracks(&self) -> Result<Vec<Track>> {
        if self.is_guest() {
            let starred = self.store.starred(crate::local::Starred::Tracks)?;
            let mut tracks = Vec::new();
            for (idx, (tid, _)) in starred.iter().enumerate() {
                if let Ok(mut trk) = self.track(tid).await {
                    trk.track_number = idx as u32 + 1;
                    tracks.push(trk);
                }
            }
            return Ok(tracks);
        }
        match self.api.liked_songs_resolved().await {
            Ok(songs) => Ok(songs
                .into_iter()
                .enumerate()
                .map(|(index, track)| wire::track(track, index as u32))
                .collect()),
            Err(_) => {
                let starred = self.store.starred(crate::local::Starred::Tracks)?;
                let mut tracks = Vec::new();
                for (idx, (tid, _)) in starred.iter().enumerate() {
                    if let Ok(mut trk) = self.track(tid).await {
                        trk.track_number = idx as u32 + 1;
                        tracks.push(trk);
                    }
                }
                Ok(tracks)
            }
        }
    }

    async fn set_track_saved(&self, track_id: &str, saved: bool) -> Result<()> {
        if self.is_guest() {
            return self.store.set_starred(crate::local::Starred::Tracks, track_id, saved);
        }
        match self.api.rate_track(track_id, saved).await {
            Ok(()) => Ok(()),
            Err(_) => self.store.set_starred(crate::local::Starred::Tracks, track_id, saved),
        }
    }

    async fn track(&self, track_id: &str) -> Result<Track> {
        let response = self
            .api
            .player_response(track_id, ytmusic::Client::Music)
            .await?;
        let details = response
            .get("videoDetails")
            .context("player response has no video details")?;
        let title = details
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let author = details
            .get("author")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let seconds: u64 = details
            .get("lengthSeconds")
            .and_then(serde_json::Value::as_str)
            .and_then(|length| length.parse().ok())
            .unwrap_or(0);
        let channel = details
            .get("channelId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let source = ytmusic::Track {
            video_id: Some(track_id.to_string()),
            title,
            artists: vec![ytmusic::ArtistRef {
                name: author,
                id: channel,
            }],
            album: None,
            duration: Some(std::time::Duration::from_secs(seconds)),
            thumbnails: Vec::new(),
            explicit: false,
            available: true,
            kind: ytmusic::TrackKind::Song,
            set_video_id: None,
            liked: None,
            views: None,
        };
        let mut track = wire::track(source, 0);
        track.cover = details
            .get("thumbnail")
            .map(|thumbnail| {
                let thumbs = collect_thumbnails(thumbnail);
                wire::cover(&thumbs)
            })
            .unwrap_or_default();
        Ok(track)
    }

    async fn track_playcount(&self, _track_id: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    async fn playlists(&self) -> Result<Vec<Playlist>> {
        let mut list = Vec::new();
        if !self.is_guest() {
            if let Ok(playlists) = self.api.library_playlists().await {
                list.extend(playlists.into_iter().filter_map(|playlist| library_playlist(playlist, &self.account)));
            }
        }
        if let Ok(stored) = self.store.list() {
            for s in stored {
                if !list.iter().any(|p| p.id == s.id) {
                    let count = self.store.tracks(&s.id).map(|t| t.len()).unwrap_or(0);
                    list.push(Playlist {
                        id: s.id,
                        name: s.name,
                        owner: if self.account.is_empty() { "Guest".to_string() } else { self.account.clone() },
                        owner_id: String::new(),
                        owned: true,
                        collaborative: false,
                        blend: false,
                        public: false,
                        cover: None,
                        track_count: count as u32,
                        modified_at: Some(s.modified_at),
                    });
                }
            }
        }
        Ok(list)
    }

    async fn create_playlist(&self, name: &str) -> Result<String> {
        if self.is_guest() {
            return self.store.create(name);
        }
        match self.api.create_playlist(name).await {
            Ok(id) => Ok(id),
            Err(_) => self.store.create(name),
        }
    }

    async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return self.store.rename(playlist_id, name);
        }
        self.api.rename_playlist(playlist_id, name).await
    }

    async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return self.store.delete(playlist_id);
        }
        self.api.delete_playlist(playlist_id).await
    }

    async fn remove_playlist_from_library(&self, playlist_id: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return self.store.delete(playlist_id);
        }
        self.api.rate_playlist(playlist_id, false).await
    }

    async fn add_playlist_to_library(&self, playlist_id: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return Ok(());
        }
        self.api.rate_playlist(playlist_id, true).await
    }

    async fn set_playlist_public(&self, playlist_id: &str, public: bool) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return Ok(());
        }
        self.api.set_playlist_privacy(playlist_id, public).await
    }

    async fn add_track_to_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return self.store.add(playlist_id, track_id);
        }
        self.api.add_playlist_track(playlist_id, track_id).await
    }

    async fn remove_track_from_playlist(&self, playlist_id: &str, track_id: &str) -> Result<()> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            return self.store.remove(playlist_id, track_id);
        }
        let tracks = self.api.playlist(playlist_id).await?.tracks;
        let seat = |list: &[ytmusic::Track]| {
            list.iter()
                .position(|track| track.video_id.as_deref() == Some(track_id))
        };
        let index = match seat(&tracks) {
            Some(index) => index,
            None => seat(&self.api.swap_playable(tracks.clone()).await)
                .context("track is not in the playlist")?,
        };
        let track = tracks.get(index).context("track is not in the playlist")?;
        let video_id = track.video_id.as_deref().context("track has no video id")?;
        let set_video_id = track
            .set_video_id
            .as_deref()
            .context("track cannot be removed from this playlist")?;
        self.api
            .remove_playlist_track(playlist_id, video_id, set_video_id)
            .await
    }

    async fn saved_albums(&self) -> Result<Vec<Album>> {
        if self.is_guest() {
            let starred = self.store.starred(crate::local::Starred::Albums)?;
            let mut albums = Vec::new();
            for (aid, _) in starred {
                if let Ok(detail) = self.album(&aid).await {
                    albums.push(detail.album);
                }
            }
            return Ok(albums);
        }
        match self.api.library_albums().await {
            Ok(albums) => Ok(albums.into_iter().map(wire::album).collect()),
            Err(_) => {
                let starred = self.store.starred(crate::local::Starred::Albums)?;
                let mut albums = Vec::new();
                for (aid, _) in starred {
                    if let Ok(detail) = self.album(&aid).await {
                        albums.push(detail.album);
                    }
                }
                Ok(albums)
            }
        }
    }

    async fn set_album_saved(&self, album_id: &str, saved: bool) -> Result<()> {
        if self.is_guest() {
            return self.store.set_starred(crate::local::Starred::Albums, album_id, saved);
        }
        let detail = match self.api.album(album_id).await {
            Ok(detail) => detail,
            Err(_) => return self.store.set_starred(crate::local::Starred::Albums, album_id, saved),
        };
        let Some(playlist_id) = detail.album.playlist_id else {
            return self.store.set_starred(crate::local::Starred::Albums, album_id, saved);
        };
        match self.api.rate_playlist(&playlist_id, saved).await {
            Ok(()) => Ok(()),
            Err(_) => self.store.set_starred(crate::local::Starred::Albums, album_id, saved),
        }
    }

    async fn saved_artists(&self) -> Result<Vec<SavedArtist>> {
        if self.is_guest() {
            let starred = self.store.starred(crate::local::Starred::Artists)?;
            let mut artists = Vec::new();
            for (aid, _) in starred {
                if let Ok(artist) = self.artist(&aid).await {
                    artists.push(SavedArtist {
                        id: aid,
                        name: artist.name,
                        cover: artist.cover_large,
                        added_at: None,
                    });
                }
            }
            return Ok(artists);
        }
        match subscriptions::saved(&self.api).await {
            Ok(artists) => Ok(artists),
            Err(_) => {
                let starred = self.store.starred(crate::local::Starred::Artists)?;
                let mut artists = Vec::new();
                for (aid, _) in starred {
                    if let Ok(artist) = self.artist(&aid).await {
                        artists.push(SavedArtist {
                            id: aid,
                            name: artist.name,
                            cover: artist.cover_large,
                            added_at: None,
                        });
                    }
                }
                Ok(artists)
            }
        }
    }

    async fn set_artist_saved(&self, artist_id: &str, saved: bool) -> Result<()> {
        if self.is_guest() {
            return self.store.set_starred(crate::local::Starred::Artists, artist_id, saved);
        }
        match subscriptions::set_saved(&self.api, artist_id, saved).await {
            Ok(()) => Ok(()),
            Err(_) => self.store.set_starred(crate::local::Starred::Artists, artist_id, saved),
        }
    }

    async fn album(&self, album_id: &str) -> Result<AlbumDetail> {
        let mut detail = self.api.album(album_id).await?;
        detail.tracks = self.api.swap_playable(detail.tracks).await;
        Ok(wire::album_detail(detail))
    }

    async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>> {
        Ok(self.album(album_id).await?.tracks)
    }

    /// The artist's own releases without the album the page is already showing. Similar
    /// artists stay out: the client library reads no similarity off artist pages.
    async fn album_catalogue(
        &self,
        album_id: &str,
        artist_id: Option<&str>,
    ) -> Result<AlbumCatalogue> {
        let Some(artist_id) = artist_id else {
            return Ok(AlbumCatalogue::default());
        };
        let artist = self
            .api
            .artist(artist_id)
            .await
            .with_context(|| format!("cannot load more from artist {artist_id}"))?;
        let also_like = artist
            .albums
            .into_iter()
            .chain(artist.singles)
            .map(wire::album)
            .filter(|album| album.id != album_id)
            .take(SUGGESTIONS)
            .collect();
        Ok(AlbumCatalogue {
            also_like,
            similar: Vec::new(),
        })
    }

    async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetail> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            let stored = self.store.one(playlist_id)?;
            let track_ids = self.store.tracks(playlist_id)?;
            let mut tracks = Vec::new();
            for (idx, tid) in track_ids.iter().enumerate() {
                if let Ok(mut trk) = self.track(tid).await {
                    trk.track_number = idx as u32 + 1;
                    tracks.push(trk);
                }
            }
            let cover = tracks.first().and_then(|t| t.cover.clone());
            return Ok(PlaylistDetail {
                playlist: Playlist {
                    id: stored.id,
                    name: stored.name,
                    owner: if self.account.is_empty() { "Guest".to_string() } else { self.account.clone() },
                    owner_id: String::new(),
                    owned: true,
                    collaborative: false,
                    blend: false,
                    public: false,
                    cover,
                    track_count: tracks.len() as u32,
                    modified_at: Some(stored.modified_at),
                },
                tracks,
                continuation: None,
            });
        }
        let mut detail = self.api.playlist_page(playlist_id).await?;
        detail.tracks = self.api.swap_playable(detail.tracks).await;
        Ok(wire::playlist_detail(detail))
    }

    async fn playlist_continuation(
        &self,
        continuation: &str,
    ) -> Result<(Vec<Track>, Option<String>)> {
        let mut page = self.api.playlist_continuation(continuation).await?;
        page.tracks = self.api.swap_playable(page.tracks).await;
        Ok(wire::playlist_page(page))
    }

    async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            let track_ids = self.store.tracks(playlist_id)?;
            let mut tracks = Vec::new();
            for (idx, tid) in track_ids.iter().enumerate() {
                if let Ok(mut trk) = self.track(tid).await {
                    trk.track_number = idx as u32 + 1;
                    tracks.push(trk);
                }
            }
            return Ok(tracks);
        }
        let mut detail = self.api.playlist(playlist_id).await?;
        detail.tracks = self.api.swap_playable(detail.tracks).await;
        Ok(wire::playlist_detail(detail).tracks)
    }

    async fn playlist_covers(&self, playlist_id: &str, wanted: usize) -> Result<Vec<String>> {
        if playlist_id.starts_with(crate::LOCAL_PLAYLIST_PREFIX) {
            let track_ids = self.store.tracks(playlist_id)?;
            let mut covers = Vec::new();
            for tid in track_ids.iter().take(wanted) {
                if let Ok(trk) = self.track(tid).await {
                    if let Some(c) = trk.cover {
                        covers.push(c);
                    }
                }
            }
            return Ok(covers);
        }
        let mut tracks = self.api.playlist_page(playlist_id).await?.tracks;
        tracks = self.api.swap_playable(tracks).await;
        let tracks: Vec<Track> = tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect();
        Ok(crate::distinct_covers(&tracks, wanted))
    }

    async fn track_radio(
        &self,
        track_id: &str,
        from: Option<&str>,
    ) -> Result<(Vec<Track>, Option<String>)> {
        radio::station(&self.api, track_id, from).await
    }

    async fn search(&self, query: &str) -> Result<Vec<Track>> {
        Ok(self
            .api
            .search_songs(query)
            .await?
            .into_iter()
            .enumerate()
            .map(|(index, track)| wire::track(track, index as u32))
            .collect())
    }

    async fn search_albums(&self, query: &str) -> Result<Vec<Album>> {
        Ok(self
            .api
            .search_albums(query)
            .await?
            .into_iter()
            .map(wire::album)
            .collect())
    }

    async fn search_playlists(&self, query: &str) -> Result<Vec<Playlist>> {
        Ok(self
            .api
            .search_playlists(query)
            .await?
            .into_iter()
            .map(|playlist| wire::playlist(playlist, false, false))
            .collect())
    }

    async fn home(&self) -> Result<HomeFeed> {
        let mut feed = genres::home(self.api.clone(), self.account.clone());
        let mut whole = HomeFeed::default();
        while let Some(lot) = feed.recv().await {
            whole = lot?;
        }
        Ok(whole)
    }

    async fn home_paged(&self) -> Result<Feed> {
        Ok(genres::home(self.api.clone(), self.account.clone()))
    }

    async fn genres(&self) -> Result<Vec<Genre>> {
        genres::genres(&self.api).await
    }

    async fn genre(&self, genre_id: &str) -> Result<GenreDetail> {
        genres::genre(&self.api, genre_id).await
    }
}

fn collect_thumbnails(node: &serde_json::Value) -> Vec<ytmusic::Thumbnail> {
    node.get("thumbnails")
        .and_then(serde_json::Value::as_array)
        .map(|thumbs| {
            thumbs
                .iter()
                .filter_map(|thumb| {
                    Some(ytmusic::Thumbnail {
                        url: thumb.get("url")?.as_str()?.to_string(),
                        width: thumb.get("width").and_then(serde_json::Value::as_u64)? as u32,
                        height: thumb.get("height").and_then(serde_json::Value::as_u64)? as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A playlist of the library as Sonora lists it: one the user made is theirs by name, and the
/// saved-episodes list YouTube keeps for podcasts is left out.
pub(crate) fn library_playlist(source: ytmusic::Playlist, account: &str) -> Option<Playlist> {
    if source.id == EPISODES {
        return None;
    }
    let mut playlist = wire::playlist(source, false, false);
    if playlist.owned && playlist.owner.is_empty() {
        playlist.owner = account.to_owned();
    }

    Some(playlist)
}
