import type { ContentType } from './wasm/iso2x.js';

/**
 * Content-type table:
 * https://free60.org/System-Software/Formats/STFS/#content-types
 */
export const contentTypeLabels: Record<ContentType, string> = {
	gamesOnDemand: 'Games on Demand',
	xboxOriginal: 'Xbox Original',
	arcadeGame: 'Arcade Game',
	xbox360Title: 'Xbox 360 Title',
	installedGame: 'Installed Game',
	gameDemo: 'Game Demo',
	communityGame: 'Community Game',
	xna: 'XNA',
	savedGame: 'Saved Game',
	marketPlaceContent: 'Marketplace Content',
	publisher: 'Publisher',
	iptvPauseBuffer: 'IPTV Pause Buffer',
	avatarAssetPack: 'Avatar Asset Pack',
	avatarItem: 'Avatar Item',
	profile: 'Profile',
	gamerPicture: 'Gamer Picture',
	theme: 'Theme',
	cacheFile: 'Cache File',
	storageDownload: 'Storage Download',
	xboxSavedGame: 'Xbox Saved Game',
	xboxDownload: 'Xbox Download',
	gamerTitle: 'Gamer Title',
	installer: 'Installer',
	gameTrailer: 'Game Trailer',
	licenseStore: 'License Store',
	movie: 'Movie',
	video: 'Video',
	tv: 'TV',
	musicVideo: 'Music Video',
	gameVideo: 'Game Video',
	podcastVideo: 'Podcast Video',
	viralVideo: 'Viral Video',
};
