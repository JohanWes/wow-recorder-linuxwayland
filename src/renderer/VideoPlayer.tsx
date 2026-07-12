import {
  AppState,
  DeathMarkers,
  RendererVideo,
  SliderMark,
  VideoMarker,
  VideoPlayerSettings,
} from 'main/types';
import {
  forwardRef,
  MutableRefObject,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { Backdrop, Box, Slider } from '@mui/material';
import PlayArrowIcon from '@mui/icons-material/PlayArrow';
import PauseIcon from '@mui/icons-material/Pause';
import VolumeUpIcon from '@mui/icons-material/VolumeUp';
import VolumeDownIcon from '@mui/icons-material/VolumeDown';
import VolumeOffIcon from '@mui/icons-material/VolumeOff';
import VolumeMuteIcon from '@mui/icons-material/VolumeMute';
import FullscreenIcon from '@mui/icons-material/Fullscreen';
import MovieIcon from '@mui/icons-material/Movie';
import ClearIcon from '@mui/icons-material/Clear';
import DoneIcon from '@mui/icons-material/Done';
import screenfull from 'screenfull';
import { ConfigurationSchema } from 'config/configSchema';
import { getLocalePhrase } from 'localisation/translations';
import DeathIcon from '../../assets/icon/death.png';
import type { ExcalidrawElement } from '@excalidraw/excalidraw/element/types';
import {
  convertNumToDeathMarkers,
  getAllDeathMarkers,
  getEncounterMarkers,
  getOwnDeathMarkers,
  getRoundMarkers,
  isClip,
  isMythicPlusUtil,
  isSoloShuffleUtil,
  secToMmSs,
} from './rendererutils';
import { Button } from './components/Button/Button';
import { Tooltip } from './components/Tooltip/Tooltip';
import { DrawingOverlay } from './components/DrawingOverlay/DrawingOverlay';
import { FolderOpen, Pencil } from 'lucide-react';
import Separator from './components/Separator/Separator';
import { Phrase } from 'localisation/phrases';
import { getMediaUrl } from './mediaUrl';

interface IProps {
  videos: RendererVideo[];
  persistentProgress: MutableRefObject<number>;
  config: ConfigurationSchema;
  appState: AppState;
  setAppState: React.Dispatch<React.SetStateAction<AppState>>;
  onVideoAspect?: (aspect: number) => void;
}

const ipc = window.electron.ipcRenderer;
const playbackRates = [0.25, 0.5, 1, 2];
const progressInterval = 100;
const seekTimeout = 2500;
const firstFrameTimeout = 250;

type VideoWithFrameCallback = HTMLVideoElement & {
  requestVideoFrameCallback?: (callback: () => void) => number;
  cancelVideoFrameCallback?: (handle: number) => void;
  fastSeek?: (time: number) => void;
};

interface SeekRequest {
  seconds: number;
  fast: boolean;
  generation: number;
}

const sliderBaseSx = {
  '& .MuiSlider-thumb': {
    color: 'white',
    width: '10px',
    height: '10px',
    '&:hover': {
      color: '#bb4220',
      boxShadow: 'none',
    },
  },
  '& .MuiSlider-track': {
    color: '#bb4220',
    height: '4px',
  },
  '& .MuiSlider-rail': {
    color: '#bb4220',
    height: '4px',
  },
  '& .MuiSlider-active': {
    color: '#bb4220',
  },
};

export interface VideoPlayerRef {
  // Exposes external seeking of the video player.
  // For example from by clicking a timestamp in chat.
  seekAllPlayersTo: (seconds: number) => void;
}

export const VideoPlayer = forwardRef<VideoPlayerRef, IProps>((props, ref) => {
  const {
    videos,
    persistentProgress,
    config,
    appState,
    setAppState,
    onVideoAspect,
  } = props;

  const { playing, multiPlayerMode, language } = appState;

  if (videos.length < 1 || videos.length > 4) {
    // Protect against stupid programmer errors.
    throw new Error('VideoPlayer should only be passed up to 4 videos');
  }

  // Keep both banks rendered. The active bank completely covers the standby
  // bank, allowing WebKitGTK to preroll real video frames without exposing its
  // flush-to-black behaviour during a seek.
  const videoBanks = useRef<(VideoWithFrameCallback | null)[][]>([
    Array(4).fill(null),
    Array(4).fill(null),
  ]);
  const [activeBank, setActiveBank] = useState<0 | 1>(0);
  const activeBankRef = useRef<0 | 1>(0);
  const readyPlayers = useRef(new Set<number>());
  const preparingPlayers = useRef(new Set<number>());
  const cancelFirstFrameWaits = useRef(new Set<() => void>());
  const durations = useRef<number[]>(Array(4).fill(0));
  const progressSlider = useRef<HTMLSpanElement>(null);

  // Progress is in seconds. Strictly it is the position of the
  // slider, which is usally the same as the video except for
  // when the user is dragging.
  const [progress, setProgress] = useState<number>(0);

  // While the user is dragging the thumb of the slider, we don't
  // want to update the video position. This is used to conditionally
  // avoid this.
  const [isDragging, setIsDragging] = useState(false);

  const [playbackRate, setPlaybackRate] = useState<number>(1);
  const [duration, setDuration] = useState<number>(0);

  // In clipping mode, the user controls three thumbs. The regular thumb
  // that controls the video position, and a start and stop thumb to
  // indicate where the clip should be made from.
  const [clipMode, setClipMode] = useState<boolean>(false);
  const [clipStartValue, setClipStartValue] = useState<number>(0);
  const [clipStopValue, setClipStopValue] = useState<number>(100);

  // This exists to force a re-render on resizing of the window, so that
  // the coloring of the progress slider remains correct across a resize.
  const [, setWidth] = useState<number>(0);

  // We show a progress spinner until the video is ready to play.
  const [spinner, setSpinner] = useState<boolean>(true);
  const [isFullscreen, setIsFullscreen] = useState(false);

  useEffect(() => {
    if (!screenfull.isEnabled) return;

    const handleFullscreenChange = () =>
      setIsFullscreen(screenfull.isFullscreen);
    screenfull.on('change', handleFullscreenChange);
    return () => screenfull.off('change', handleFullscreenChange);
  }, []);

  // On the initial seek we will attempt to resume playback from the
  // persistentProgress prop. The ideas is that when switching between
  // different POVs of the same activity we want to play from the same
  // point.
  const timestamp = useRef(`#t=${persistentProgress.current}`).current;

  const diskVideo = videos.find((v) => !v.cloud) ?? videos[0];
  const clippable = !multiPlayerMode && diskVideo !== undefined;

  // Deliberatly don't update the source when the timestamp changes. That's
  // just the initial playhead position. We only care to change sources when
  // the videos we are meant to be playing changes.
  const [srcs, setSrcs] = useState<string[]>([]);
  useEffect(() => {
    let cancelled = false;
    Promise.all(videos.map((video) => getMediaUrl(video.videoSource))).then(
      (urls) => {
        if (!cancelled) setSrcs(urls.map((url) => url + timestamp));
      },
      (error) => console.error('Video Player URL Error', error),
    );
    return () => {
      cancelled = true;
    };
  }, [videos, timestamp]);

  // Read and store the video player state of 'volume' and 'muted' so that we may
  // restore it when selecting a different video. This config gets stored as a
  // variable in the main process that we update and retrieve, but is not written
  // to config so is lost on app restart.
  const videoPlayerSettings = ipc.sendSync('videoPlayerSettings', [
    'get',
  ]) as VideoPlayerSettings;

  const [volume, setVolume] = useState<number>(videoPlayerSettings.volume);
  const [muted, setMuted] = useState<boolean>(videoPlayerSettings.muted);

  const [isDrawingEnabled, setIsDrawingEnabled] = useState(false);
  const [, setDrawingElements] = useState<readonly ExcalidrawElement[]>([]);

  /**
   * Set if the video is playing or not.
   */
  const setPlaying = useCallback(
    (v: boolean) => {
      playingRef.current = v;
      setAppState((prevState) => {
        return {
          ...prevState,
          playing: v,
        };
      });
    },
    [setAppState],
  );

  const playingRef = useRef(playing);
  const volumeRef = useRef(volume);
  const mutedRef = useRef(muted);
  const playbackRateRef = useRef(playbackRate);
  playingRef.current = playing;
  volumeRef.current = volume;
  mutedRef.current = muted;
  playbackRateRef.current = playbackRate;

  const seekGeneration = useRef(0);
  const queuedSeek = useRef<SeekRequest | null>(null);
  const seekInFlight = useRef(false);
  const cancelSeekWaits = useRef(new Set<() => void>());

  const waitForSeekFrame = (
    video: VideoWithFrameCallback,
    request: SeekRequest,
  ) =>
    new Promise<void>((resolve, reject) => {
      let frameHandle: number | undefined;
      let settled = false;
      const timeout = window.setTimeout(() => {
        // Give up waiting but still commit the swap: one possibly stale
        // frame beats snapping playback back to the pre-seek position.
        console.warn('Timed out waiting for a decoded seek frame');
        finish();
      }, seekTimeout);

      const finish = (error?: Error) => {
        if (settled) return;
        settled = true;
        cancelSeekWaits.current.delete(cancel);
        window.clearTimeout(timeout);
        video.removeEventListener('seeked', onSeeked);
        video.removeEventListener('error', onError);
        if (frameHandle !== undefined) {
          video.cancelVideoFrameCallback?.(frameHandle);
        }
        if (error) reject(error);
        else resolve();
      };
      const cancel = () => finish();
      const onError = () => finish(new Error('Video failed while seeking'));
      const finishAfterFrame = () => {
        if (request.generation !== seekGeneration.current) {
          finish();
          return;
        }
        if (video.requestVideoFrameCallback) {
          frameHandle = video.requestVideoFrameCallback(() => finish());
        } else {
          requestAnimationFrame(() => requestAnimationFrame(() => finish()));
        }
      };
      const onSeeked = () => finishAfterFrame();

      cancelSeekWaits.current.add(cancel);
      video.addEventListener('seeked', onSeeked, { once: true });
      video.addEventListener('error', onError, { once: true });
      const target = Math.min(
        Math.max(0, request.seconds),
        Number.isFinite(video.duration) ? video.duration : request.seconds,
      );
      if (
        Math.abs(video.currentTime - target) < 0.001 &&
        video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA
      ) {
        finishAfterFrame();
        return;
      }
      if (request.fast && video.fastSeek) video.fastSeek(target);
      else video.currentTime = target;
    });

  const performSeek = async (request: SeekRequest) => {
    const currentBank = activeBankRef.current;
    const nextBank = currentBank === 0 ? 1 : 0;
    const currentVideos = videoBanks.current[currentBank].slice(
      0,
      videos.length,
    );
    const nextVideos = videoBanks.current[nextBank].slice(0, videos.length);
    if (
      currentVideos.some((video) => !video) ||
      nextVideos.some((video) => !video)
    ) {
      return;
    }

    currentVideos.forEach((video) => video?.pause());
    nextVideos.forEach((video) => {
      if (!video) return;
      video.pause();
      video.muted = true;
      video.volume = volumeRef.current;
      video.playbackRate = playbackRateRef.current;
    });

    const bankRequest = {
      ...request,
      // Independent keyframe layouts can make fastSeek land each POV at a
      // different moment. Exact seeks preserve grid synchronization.
      fast: request.fast && videos.length === 1,
    };
    await Promise.all(
      nextVideos.map((video) =>
        waitForSeekFrame(video as VideoWithFrameCallback, bankRequest),
      ),
    );
    if (request.generation !== seekGeneration.current) return;

    nextVideos.forEach((video, index) => {
      if (!video) return;
      video.muted = index !== 0 || mutedRef.current;
    });
    currentVideos.forEach((video) => {
      if (!video) return;
      video.pause();
      video.muted = true;
    });

    activeBankRef.current = nextBank;
    setActiveBank(nextBank);
    const committedTime = nextVideos[0]?.currentTime ?? request.seconds;
    persistentProgress.current = Math.max(0, committedTime);
    setProgress(Math.max(0, committedTime));

    if (playingRef.current) {
      nextVideos.forEach((video) => void video?.play().catch(onError));
    }
  };

  const syncActivePlayback = () => {
    videoBanks.current[activeBankRef.current]
      .slice(0, videos.length)
      .forEach((video, index) => {
        if (!video) return;
        video.volume = volumeRef.current;
        video.playbackRate = playbackRateRef.current;
        video.muted = index !== 0 || mutedRef.current;
        if (playingRef.current) void video.play().catch(onError);
        else video.pause();
      });
  };

  const banksReady = () =>
    videoBanks.current.every((bank) =>
      bank.slice(0, videos.length).every((video) => video),
    );

  const drainSeekQueue = async () => {
    if (seekInFlight.current) return;
    seekInFlight.current = true;
    try {
      // A request that arrives before both banks have mounted stays queued;
      // the video ref callbacks re-run the drain once the elements exist.
      while (queuedSeek.current && banksReady()) {
        const request = queuedSeek.current;
        queuedSeek.current = null;
        try {
          await performSeek(request);
        } catch (error) {
          if (request.generation === seekGeneration.current) {
            console.error('Video seek failed', error);
          }
        }
      }
    } finally {
      seekInFlight.current = false;
      syncActivePlayback();
    }
  };

  const seekPlayersTo = (seconds: number, fast: boolean) => {
    const generation = ++seekGeneration.current;
    cancelSeekWaits.current.forEach((cancel) => cancel());
    cancelSeekWaits.current.clear();
    queuedSeek.current = { seconds, fast, generation };
    void drainSeekQueue();
  };

  useImperativeHandle(ref, () => ({
    seekAllPlayersTo(seconds: number) {
      seekPlayersTo(seconds, true);
    },
  }));

  /**
   * Return a death marker appropriate for the MUI slider component.
   */
  const getDeathMark = (marker: VideoMarker): SliderMark => {
    return {
      value: marker.time,
      label: (
        <Tooltip content={marker.text}>
          <Box
            component="img"
            src={DeathIcon}
            sx={{
              p: '1px',
              height: '13px',
              width: '13px',
              objectFit: 'fill',
            }}
          />
        </Tooltip>
      ),
    };
  };

  /**
   * Get the video timeline markers appropriate for the current video and
   * configuration.
   */
  // Memoized: the progress poll re-renders this component at 10Hz during
  // playback, and rebuilding the death marker tooltips every tick is waste.
  const timelineMarks = useMemo(() => {
    const marks: SliderMark[] = [];

    if (duration === 0 || isClip(videos[0])) {
      return marks;
    }

    const deathMarkerConfig = convertNumToDeathMarkers(config.deathMarkers);

    if (deathMarkerConfig === DeathMarkers.ALL) {
      getAllDeathMarkers(videos[0], language)
        .map(getDeathMark)
        .forEach((m) => marks.push(m));
    } else if (deathMarkerConfig === DeathMarkers.OWN) {
      getOwnDeathMarkers(videos[0], language)
        .map(getDeathMark)
        .forEach((m) => marks.push(m));
    }

    return marks;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [duration, videos, config.deathMarkers, language]);

  /**
   * Return all the active video markers given the current video and config.
   */
  const getActiveMarkers = () => {
    const activeMarkers: VideoMarker[] = [];

    if (isMythicPlusUtil(videos[0]) && config.encounterMarkers) {
      getEncounterMarkers(videos[0]).forEach((m) => activeMarkers.push(m));
    }

    if (isSoloShuffleUtil(videos[0]) && config.roundMarkers) {
      getRoundMarkers(videos[0]).forEach((m) => activeMarkers.push(m));
    }

    return activeMarkers;
  };

  /**
   * Build a linear gradient CSS property from a list of video makers.
   * Returned string is something of the form:
   *   "linear-gradient(90deg, rgba(1, 1, 1, 1) 0px, rgba(1, 1, 1, 1) 10px, ... max)".
   */
  const markersToLinearGradient = (
    markers: VideoMarker[],
    fillerColor: string,
  ) => {
    if (!progressSlider.current || duration === 0 || isClip(videos[0])) {
      // Initial render shows a flash of the default color without this,
      // and this branch also protects us loading anything on the clips
      // category where the markers are bogus as they are just lifted
      // from the parent.
      return `linear-gradient(90deg, ${fillerColor} 0%, ${fillerColor} 100%)`;
    }

    let ptr = 0;
    const gradients = [];
    const sliderWidth = progressSlider.current.getBoundingClientRect().width;
    const pxToSecRatio = sliderWidth / duration;

    markers
      .sort((a, b) => a.time - b.time) // Chronological sort
      .forEach((marker) => {
        if (ptr !== marker.time) {
          // If we've not moved the pointer to this point yet, then add a
          // filler block to the gradient.
          const start = Math.round(ptr * pxToSecRatio);
          const end = Math.round(marker.time * pxToSecRatio);
          gradients.push(`${fillerColor} ${start}px`);
          gradients.push(`${fillerColor} ${end}px`);
        }

        // The pointer must have caught up now, so add the current marker.
        const start = Math.round(marker.time * pxToSecRatio);
        const end = Math.round((marker.time + marker.duration) * pxToSecRatio);
        gradients.push(`${marker.color} ${start}px`);
        gradients.push(`${marker.color} ${end}px`);

        // Move the pointer on.
        ptr = marker.time + marker.duration;
      });

    // If we didn't reach the end, add filler to there. We don't want the
    // last gradient to continue to the end.
    if (ptr !== duration) {
      const start = Math.round(ptr * pxToSecRatio);
      gradients.push(`${fillerColor} ${start}px`);
      gradients.push(`${fillerColor} ${sliderWidth}px`);
    }

    // Build the string from the list of colors and locations.
    const gradient = `linear-gradient(90deg, ${gradients.join(', ')})`;
    return gradient;
  };

  /**
   * Get a linear gradient style for the video rail for the encounter (M+ only)
   * and round (Solo Shuffle only) markers.
   */
  const getRailGradient = () => {
    const fillerColor = '#5A2F27';
    const activeMarkers = getActiveMarkers();
    return markersToLinearGradient(activeMarkers, fillerColor);
  };

  /**
   * Get a linear gradient for the video track for the encounter (M+ only)
   * and round (Solo Shuffle only) markers.
   */
  const getTrackGradient = () => {
    const fillerColor = '#BB4420';
    const activeMarkers = getActiveMarkers();

    // Lower the opacity of everything in the linear gradient otherwise it
    // looks out of place on the slider track. This doesn't need to happen
    // on the slider rail as it has blanket low opacity. Makes a replacement
    // like: "rgba(0, 0, 0, 1) -> rgba(0, 0, 0, 0.4)"
    return markersToLinearGradient(activeMarkers, fillerColor).replace(
      /, 1\)/g,
      ', 0.4)',
    );
  };

  /**
   * Conveince method to get an appropriate sx prop for the regular
   * progress slider.
   */
  const getProgressSliderSx = () => {
    return {
      ...sliderBaseSx,
      m: 2,
      width: '100%',
      '& .MuiSlider-markLabel': {
        top: '20px',
      },
      '& .MuiSlider-mark': {
        backgroundColor: 'white',
        width: '2px',
        height: '4px',
      },
      '& .MuiSlider-rail': {
        background: getRailGradient(),
        height: '4px',
      },
      '& .MuiSlider-track': {
        background: getTrackGradient(),
        border: 'none',
        height: '4px',
      },
    };
  };

  /**
   * Conveince method to get an appropriate sx prop for the clip mode
   * progress slider.
   */
  const getProgressClipSliderSx = () => {
    return {
      ...sliderBaseSx,
      m: 2,
      width: '100%',
      '& .MuiSlider-thumb': {
        "&[data-index='0']": {
          backgroundColor: 'white',
          width: '5px',
          height: '20px',
          borderRadius: 0,
          '& .MuiSlider-valueLabel': {
            fontSize: '0.75rem',
            transform: 'translate(-43%, -100%)', // This moves the whole label.
            '&::before': {
              transform: 'translate(460%, 40%) rotate(45deg)', // This moves the notch.
            },
          },
          '&:hover': {
            backgroundColor: '#bb4220',
            boxShadow: 'none',
          },
        },
        "&[data-index='1']": {
          width: '10px',
          height: '10px',
          zIndex: 1,
          backgroundColor: 'white',
          '& .MuiSlider-valueLabel': {
            fontSize: '0.75rem',
            rotate: '180deg',
            transform: 'translateY(-15%) scale(1)',
            '& .MuiSlider-valueLabelCircle': {
              rotate: '180deg',
            },
          },
          '&:hover': {
            backgroundColor: '#bb4220',
            boxShadow: 'none',
          },
        },
        "&[data-index='2']": {
          backgroundColor: 'white',
          width: '5px',
          height: '20px',
          borderRadius: 0,
          '& .MuiSlider-valueLabel': {
            fontSize: '0.75rem',
            transform: 'translate(43%, -100%)', // This moves the whole label.
            '&::before': {
              transform: 'translate(-525%, 40%) rotate(45deg)', // This moves the notch.
            },
          },
          '&:hover': {
            backgroundColor: '#bb4220',
            boxShadow: 'none',
          },
        },
      },
    };
  };

  /** Toggle playback for the active video bank. */
  const togglePlaying = () => {
    setPlaying(!playingRef.current);
  };

  // By default the window hijacks media keys even when
  // the window isn't focused or it is minimized
  // so we override the action handlers
  useEffect(() => {
    ipc.on('window-focus-status', (arg: unknown) => {
      const focused = arg as boolean;
      if (focused) {
        // unset action handlers when focused, making them work like initially
        navigator.mediaSession.setActionHandler('play', null);
        navigator.mediaSession.setActionHandler('pause', null);
      } else {
        navigator.mediaSession.setActionHandler('play', () => {});
        navigator.mediaSession.setActionHandler('pause', () => {});
      }
      // note that this kind of solution doesn't work for the stop key for some reason.
      // it seems to behave differently and it clears the entire session
    });
    return () => {
      ipc.removeAllListeners('window-focus-status');
    };
  }, []);

  /**
   * Handle the user clicking on the rate button by going to the next rate
   * option.
   */
  const handleRateChange = () => {
    const index = playbackRates.indexOf(playbackRate);

    if (index === playbackRates.length - 1) {
      setPlaybackRate(playbackRates[0]);
    } else {
      setPlaybackRate(playbackRates[index + 1]);
    }
  };

  /**
   * Handle a click from the user on the progress slider by seeking to that
   * position.
   */
  const handleProgressSliderChange = (
    _event: Event,
    value: number | number[],
    index: number,
  ) => {
    if (Array.isArray(value)) {
      setClipStartValue(value[0]);
      setClipStopValue(value[2]);

      if (index === 1) {
        setProgress(value[1]);
      }
    }

    if (typeof value === 'number') {
      setProgress(value);
    }
  };

  const handleChangeCommitted = (
    _event: React.SyntheticEvent | Event,
    value: number | number[],
  ) => {
    setIsDragging(false);

    if (Array.isArray(value) && typeof value[1] == 'number') {
      seekPlayersTo(value[1], true);
    }

    if (typeof value === 'number') {
      seekPlayersTo(value, true);
    }
  };

  /**
   * Handle a mouse down event for the slider.
   */
  const onSliderMouseDown = () => {
    setIsDragging(true);

    if (multiPlayerMode) {
      // Force a pause in multi player mode to avoid any risk of video
      // desync or weird slider behaviour.
      setPlaying(false);
    }
  };

  /**
   * Enter / exit fullscreen mode.
   */
  const toggleFullscreen = () => {
    const playerElement = document.getElementById('player-and-controls');

    if (playerElement) {
      screenfull.toggle(playerElement);
    }
  };

  const onLoadedMetadata = (
    bank: number,
    index: number,
    video: HTMLVideoElement,
  ) => {
    if (bank !== 0) return;
    durations.current[index] = video.duration;
    const knownDurations = durations.current.slice(0, videos.length);
    if (knownDurations.every((value) => Number.isFinite(value) && value > 0)) {
      setDuration(Math.max(...knownDurations));
    }
    if (index === 0 && video.videoWidth > 0 && video.videoHeight > 0) {
      onVideoAspect?.(video.videoWidth / video.videoHeight);
    }
  };

  const onFirstFrame = (
    bank: number,
    index: number,
    video: VideoWithFrameCallback,
  ) => {
    if (bank !== 0) return;
    if (
      readyPlayers.current.has(index) ||
      preparingPlayers.current.has(index)
    ) {
      return;
    }

    preparingPlayers.current.add(index);
    let settled = false;
    let frameHandle: number | undefined;
    let animationFrameHandle: number | undefined;
    const timeout = window.setTimeout(() => markReady(), firstFrameTimeout);

    const cancel = () => {
      if (settled) return;
      settled = true;
      cancelFirstFrameWaits.current.delete(cancel);
      window.clearTimeout(timeout);
      if (frameHandle !== undefined) {
        video.cancelVideoFrameCallback?.(frameHandle);
      }
      if (animationFrameHandle !== undefined) {
        cancelAnimationFrame(animationFrameHandle);
      }
    };

    const markReady = () => {
      if (settled) return;
      cancel();
      preparingPlayers.current.delete(index);
      if (videoBanks.current[bank][index] !== video) return;
      readyPlayers.current.add(index);
      if (readyPlayers.current.size >= videos.length) setSpinner(false);
    };
    cancelFirstFrameWaits.current.add(cancel);

    // loadeddata guarantees frame data exists, but WebKitGTK can still paint
    // one black frame before presenting it. Keep the loading cover in place
    // until the browser confirms that the first frame was actually rendered.
    // A bounded fallback is required because a paused frame may have been
    // presented just before this event handler registered the callback.
    if (video.requestVideoFrameCallback) {
      frameHandle = video.requestVideoFrameCallback(markReady);
    } else {
      animationFrameHandle = requestAnimationFrame(() => {
        animationFrameHandle = requestAnimationFrame(markReady);
      });
    }
  };

  /**
   * A video player error. Maybe should pass this through to the actual
   * log file for debug sake? Occasionally see R2 give a 503 when loading
   * videos. Don't know why, and goes away on retry. Maybe can make that
   * retry happen automatically?
   */
  const onError = (e: unknown) => {
    console.error('Video Player Error', e);
  };

  /**
   * Format the clip mode labels.
   */
  const getClipLabelFormat = (value: number, index: number) => {
    if (clipMode) {
      if (index === 0)
        return `${getLocalePhrase(language, Phrase.Start)} (${secToMmSs(value)})`;
      if (index === 1) return secToMmSs(value);
      if (index === 2)
        return `${getLocalePhrase(language, Phrase.End)} (${secToMmSs(value)})`;
    }

    return secToMmSs(value);
  };

  /**
   * Returns the progress slider for the video controls.
   */
  const renderProgressSlider = () => {
    const sx = clipMode ? getProgressClipSliderSx() : getProgressSliderSx();

    const value = clipMode
      ? [clipStartValue, progress, clipStopValue]
      : progress;

    const valueLabelFormat = clipMode ? getClipLabelFormat : secToMmSs;
    const valueLabelDisplay = clipMode ? 'on' : 'off';
    const marks = clipMode ? undefined : timelineMarks;

    return (
      <Slider
        ref={progressSlider}
        sx={sx}
        value={value}
        valueLabelFormat={valueLabelFormat}
        valueLabelDisplay={valueLabelDisplay}
        onChange={handleProgressSliderChange}
        onChangeCommitted={handleChangeCommitted}
        onMouseDown={onSliderMouseDown}
        onKeyDown={(e) => {
          // Don't have keys interact with the slider directly. This lets
          // arrow keys seek as if the video player is in focus.
          e.preventDefault();
        }}
        max={duration}
        marks={marks}
        step={0.01}
      />
    );
  };

  const renderPlayer = (bank: 0 | 1, src: string, index: number) => {
    return (
      <video
        ref={(video) => {
          videoBanks.current[bank][index] = video;
          if (video && queuedSeek.current) void drainSeekQueue();
        }}
        key={`${bank}-${src}`}
        src={src}
        className="h-full w-full bg-black object-contain"
        // The standby bank only needs metadata up front; the next seek pulls
        // in the frames it needs. Full preload would download and buffer
        // every video twice through the media server.
        preload={bank === activeBank ? 'auto' : 'metadata'}
        playsInline
        muted={bank !== activeBank || index !== 0 || muted}
        onClick={togglePlaying}
        onDoubleClick={toggleFullscreen}
        onLoadedMetadata={(event) =>
          onLoadedMetadata(bank, index, event.currentTarget)
        }
        onLoadedData={(event) => onFirstFrame(bank, index, event.currentTarget)}
        onEnded={
          index === 0
            ? () => {
                if (bank === activeBankRef.current) setPlaying(false);
              }
            : undefined
        }
        onError={onError}
      />
    );
  };

  /**
   * Returns the play/pause button for the video controls.
   */
  const renderPlayPause = () => {
    return (
      <Button variant="ghost" size="xs" onClick={togglePlaying}>
        {playing && <PauseIcon sx={{ color: 'white', fontSize: '22px' }} />}
        {!playing && (
          <PlayArrowIcon sx={{ color: 'white', fontSize: '22px' }} />
        )}
      </Button>
    );
  };

  /**
   * Toggles if the volume is muted.
   */
  const toggleMuted = () => {
    setMuted(!muted);
  };

  /**
   * Return an appropriate volume icon for the muted and volume state.
   */
  const getAppropriateVolumeIcon = () => {
    if (muted) {
      return <VolumeOffIcon sx={{ color: 'white', fontSize: '22px' }} />;
    }

    if (volume === 0) {
      return <VolumeMuteIcon sx={{ color: 'white', fontSize: '22px' }} />;
    }

    if (volume < 0.5) {
      return <VolumeDownIcon sx={{ color: 'white', fontSize: '22px' }} />;
    }

    return <VolumeUpIcon sx={{ color: 'white', fontSize: '22px' }} />;
  };

  /**
   * Returns the volume button for the video controls.
   */
  const renderVolumeButton = () => {
    return (
      <Button variant="ghost" size="xs" onClick={toggleMuted}>
        {getAppropriateVolumeIcon()}
      </Button>
    );
  };

  /**
   * Returns the progress text indicator for the video controls.
   */
  const renderProgressText = () => {
    const max = duration;

    return (
      <div className="mx-1 flex">
        <span className="whitespace-nowrap text-foreground-lighter text-[11px] font-semibold font-mono">
          {secToMmSs(progress)} / {secToMmSs(max)}
        </span>
      </div>
    );
  };

  /**
   * Returns the playback rate button for the video controls.
   */
  const renderPlaybackRateButton = () => {
    const playbackRateText = `${playbackRate}x`;

    return (
      <Tooltip content={getLocalePhrase(language, Phrase.PlaybackSpeedTooltip)}>
        <Button
          variant="ghost"
          size="xs"
          onClick={handleRateChange}
          className="whitespace-nowrap text-foreground-lighter text-[11px] font-semibold font-mono"
        >
          {playbackRateText}
        </Button>
      </Tooltip>
    );
  };

  /**
   * Open the folder containing the video.
   */
  const openLocation = (event: React.SyntheticEvent) => {
    event.stopPropagation();
    if (!diskVideo) return;

    window.electron.ipcRenderer.sendMessage('videoButton', [
      'open',
      diskVideo.videoSource,
      false,
    ]);
  };

  /**
   * Render the open folder button.
   */
  const renderOpenFolderButton = () => {
    return (
      <Tooltip
        content={getLocalePhrase(language, Phrase.OpenFolderButtonTooltip)}
      >
        <div>
          <Button
            variant="ghost"
            size="xs"
            onClick={openLocation}
            disabled={diskVideo === undefined}
          >
            <FolderOpen size={20} color="white" />
          </Button>
        </div>
      </Tooltip>
    );
  };

  /**
   * Returns the playback rate button for the video controls.
   */
  const renderClipButton = () => {
    const color = clippable ? 'white' : 'rgba(239, 239, 240, 0.25)';
    const tooltip = clippable
      ? getLocalePhrase(language, Phrase.ClipTooltip)
      : getLocalePhrase(language, Phrase.ClipUnavailableTooltip);

    return (
      <Tooltip content={tooltip}>
        <div>
          <Button
            variant="ghost"
            size="xs"
            onClick={() => {
              setClipStartValue(Math.max(0, progress - 15));
              setClipStopValue(Math.min(duration, progress + 15));
              setClipMode(true);
            }}
          >
            <MovieIcon sx={{ color, fontSize: '22px' }} />
          </Button>
        </div>
      </Tooltip>
    );
  };

  /**
   * Make a request to the main process to clip a video.
   */
  const doClip = () => {
    if (!diskVideo) return;
    const clipDuration = clipStopValue - clipStartValue;
    const clipOffset = clipStartValue;
    ipc.clipVideo(diskVideo, clipOffset, clipDuration);
    setClipMode(false);
  };

  /**
   * Render the button to end the clipping session.
   */
  const renderClipFinishedButton = () => {
    return (
      <Tooltip content={getLocalePhrase(language, Phrase.ConfirmTooltip)}>
        <Button variant="ghost" size="xs" onClick={doClip}>
          <DoneIcon sx={{ color: 'white' }} />
        </Button>
      </Tooltip>
    );
  };

  /**
   * Render the cancel clipping mode button.
   */
  const renderClipCancelButton = () => {
    return (
      <Tooltip content={getLocalePhrase(language, Phrase.CancelTooltip)}>
        <Button variant="ghost" size="xs" onClick={() => setClipMode(false)}>
          <ClearIcon sx={{ color: 'white' }} />
        </Button>
      </Tooltip>
    );
  };

  /**
   * Returns the fullscreen button for the video controls.
   */
  const renderFullscreenButton = () => {
    return (
      <Tooltip content={getLocalePhrase(language, Phrase.FullScreenTooltip)}>
        <Button variant="ghost" size="xs" onClick={toggleFullscreen}>
          <FullscreenIcon sx={{ color: 'white' }} />
        </Button>
      </Tooltip>
    );
  };

  /**
   * Handle a change event from the volume slider.
   */
  const handleVolumeChange = (_event: Event, value: number | number[]) => {
    if (typeof value === 'number') {
      setMuted(false);
      setVolume(value / 100);
    }
  };

  /**
   * Returns the volume slider.
   */
  const renderVolumeSlider = () => {
    return (
      <Slider
        sx={{ m: 1, width: '75px', ...sliderBaseSx }}
        value={muted ? 0 : volume * 100}
        onChange={handleVolumeChange}
        valueLabelFormat={Math.round}
        valueLabelDisplay="auto"
        onKeyDown={(e) => {
          e.preventDefault();
        }}
      />
    );
  };

  /**
   * Returns the drawing button for the video controls.
   */
  const renderDrawingButton = () => (
    <Tooltip content={getLocalePhrase(language, Phrase.ToggleDrawingMode)}>
      <Button
        variant="ghost"
        size="xs"
        onClick={() => setIsDrawingEnabled(!isDrawingEnabled)}
      >
        <Pencil size={20} color={isDrawingEnabled ? '#bb4420' : 'white'} />
      </Button>
    </Tooltip>
  );

  /**
   * Returns the entire video control component.
   */
  const renderControls = () => {
    return (
      <div
        className={`w-full h-10 flex flex-row justify-center items-center bg-background-dark-gradient-to border border-background-dark-gradient-to px-1 py-2 rounded-br-sm ${
          isFullscreen ? 'absolute bottom-0 left-0 z-10 bg-black/70' : ''
        }`}
      >
        {renderPlayPause()}
        {renderVolumeButton()}
        {renderVolumeSlider()}
        {renderProgressSlider()}
        {renderProgressText()}
        {!multiPlayerMode && !clipMode && (
          <Separator className="mx-2" orientation="vertical" />
        )}
        {!multiPlayerMode && !clipMode && renderOpenFolderButton()}
        <Separator className="mx-2" orientation="vertical" />
        {renderDrawingButton()}
        {!clipMode && !isClip(videos[0]) && renderClipButton()}
        {!multiPlayerMode && !clipMode && (
          <Separator className="mx-2" orientation="vertical" />
        )}
        {!clipMode && renderPlaybackRateButton()}
        {!clipMode && renderFullscreenButton()}
        {clipMode && renderClipFinishedButton()}
        {clipMode && renderClipCancelButton()}
      </div>
    );
  };

  /**
   * Handle a key down event. It would be nice to pass a "onKeyDown" react
   * callback to the player / controls box, but the player seems to swallow
   * such events, so instead we do this.
   */
  const handleKeyDown = (e: KeyboardEvent) => {
    const primary = videoBanks.current[activeBankRef.current][0];

    if (!primary) return;

    if (e.key === 'k' || e.key === ' ') {
      togglePlaying();
      e.preventDefault();
    }

    if (e.key === 'j' || e.key === 'ArrowLeft') {
      seekPlayersTo(primary.currentTime - 5, true);
    }

    if (e.key === 'l' || e.key === 'ArrowRight') {
      seekPlayersTo(primary.currentTime + 5, true);
    }

    if (e.key === '.') {
      const frame = 1 / 30; // Assume 30fps, not the end of the world if we skip 2 frames.

      seekPlayersTo(primary.currentTime + frame, false);
    }

    if (e.key === ',') {
      const frame = 1 / 30; // Assume 30fps, not the end of the world if we skip 2 frames.

      seekPlayersTo(primary.currentTime - frame, false);
    }
  };

  // Listener for keydown events when the player is open.
  useEffect(() => {
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  // This hook updates some state to force a re-render on window resize,
  // otherwise resizing the window (and hence the progress bar) causes
  // all the makers to be offset until next render.
  useLayoutEffect(() => {
    const updateWidth = () => {
      setWidth(window.innerWidth);
    };

    window.addEventListener('resize', updateWidth);
    return () => window.removeEventListener('resize', updateWidth);
  }, []);

  useEffect(() => {
    if (seekInFlight.current) return;
    syncActivePlayback();
  }, [activeBank, playing, srcs, videos.length]);

  useEffect(() => {
    videoBanks.current.forEach((bank, bankIndex) => {
      bank.slice(0, videos.length).forEach((video, index) => {
        if (!video) return;
        video.volume = volume;
        video.muted = bankIndex !== activeBank || index !== 0 || muted;
      });
    });
  }, [activeBank, muted, srcs, videos.length, volume]);

  useEffect(() => {
    videoBanks.current.forEach((bank) =>
      bank
        .slice(0, videos.length)
        .forEach((video) => video && (video.playbackRate = playbackRate)),
    );
  }, [playbackRate, srcs, videos.length]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (seekInFlight.current) return;
      const primary = videoBanks.current[activeBankRef.current][0];
      if (!primary) return;
      persistentProgress.current = primary.currentTime;
      if (!isDragging) setProgress(primary.currentTime);
    }, progressInterval);
    return () => window.clearInterval(timer);
  }, [isDragging, persistentProgress, srcs]);

  useEffect(
    () => () => {
      seekGeneration.current++;
      cancelSeekWaits.current.forEach((cancel) => cancel());
      cancelSeekWaits.current.clear();
      cancelFirstFrameWaits.current.forEach((cancel) => cancel());
      cancelFirstFrameWaits.current.clear();
      videoBanks.current.forEach((bank) =>
        bank.forEach((video) => video?.pause()),
      );
    },
    [],
  );

  // Inform the main process of a volume or muted state change.
  useEffect(() => {
    const soundSettings: VideoPlayerSettings = { volume, muted };
    ipc.sendMessage('videoPlayerSettings', ['set', soundSettings]);
  }, [volume, muted]);

  // Used to pause when the app is minimized to the system tray.
  useEffect(() => {
    ipc.on('pausePlayer', () => setPlaying(false));

    return () => {
      ipc.removeAllListeners('pausePlayer');
    };
  }, [setPlaying]);

  let playerDivClass = 'w-full h-full ';

  if (srcs.length === 2) {
    playerDivClass += 'grid grid-cols-2 grid-rows-1';
  } else if (srcs.length === 3) {
    playerDivClass += 'grid grid-cols-2 grid-rows-2';
  } else if (srcs.length === 4) {
    playerDivClass += 'grid grid-cols-2 grid-rows-2';
  }

  const renderDrawingOverlay = () => {
    return (
      <div className="absolute top-0 left-0 z-[2] w-full h-full">
        <DrawingOverlay
          isDrawingEnabled={isDrawingEnabled}
          onDrawingChange={setDrawingElements}
          appState={appState}
        />
      </div>
    );
  };

  const renderLoadingSpinner = () => {
    return (
      <Backdrop
        sx={{
          position: 'absolute',
          top: 0,
          left: 0,
          right: 0,
          bottom: 0,
          zIndex: 3,
          // Do not expose the old container geometry or an undecoded frame
          // while a newly selected video is being prepared.
          backgroundColor: 'black',
        }}
        open={spinner}
        transitionDuration={0}
      />
    );
  };

  return (
    <div id="player-and-controls" className="relative w-full h-full">
      <div style={{ height: isFullscreen ? '100%' : 'calc(100% - 40px)' }}>
        <div className="w-full h-full relative">
          {([0, 1] as const).map((bank) => (
            <div
              key={bank}
              className={`${playerDivClass} absolute inset-0`}
              style={{
                zIndex: bank === activeBank ? 1 : 0,
                pointerEvents: bank === activeBank ? 'auto' : 'none',
              }}
            >
              {srcs.map((src, index) => renderPlayer(bank, src, index))}
            </div>
          ))}
          {isDrawingEnabled && renderDrawingOverlay()}
          {renderLoadingSpinner()}
        </div>
      </div>

      {renderControls()}
    </div>
  );
});
VideoPlayer.displayName = 'VideoPlayer';

export default VideoPlayer;
