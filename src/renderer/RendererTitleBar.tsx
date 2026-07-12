import { ComponentProps } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { cn } from './components/utils';
import icon from '../../assets/icon.png';

const ipc = window.electron.ipcRenderer;
const appWindow = getCurrentWindow();

type ResizeDirection =
  | 'North'
  | 'NorthEast'
  | 'East'
  | 'SouthEast'
  | 'South'
  | 'SouthWest'
  | 'West'
  | 'NorthWest';

const resizeHandles: Array<{
  direction: ResizeDirection;
  className: string;
}> = [
  {
    direction: 'North',
    className: 'top-0 left-2 right-2 h-1.5 cursor-n-resize',
  },
  {
    direction: 'East',
    className: 'top-2 right-0 bottom-2 w-1.5 cursor-e-resize',
  },
  {
    direction: 'South',
    className: 'bottom-0 left-2 right-2 h-1.5 cursor-s-resize',
  },
  {
    direction: 'West',
    className: 'top-2 left-0 bottom-2 w-1.5 cursor-w-resize',
  },
  {
    direction: 'NorthEast',
    className: 'top-0 right-0 h-3 w-3 cursor-ne-resize',
  },
  {
    direction: 'SouthEast',
    className: 'bottom-0 right-0 h-3 w-3 cursor-se-resize',
  },
  {
    direction: 'SouthWest',
    className: 'bottom-0 left-0 h-3 w-3 cursor-sw-resize',
  },
  {
    direction: 'NorthWest',
    className: 'top-0 left-0 h-3 w-3 cursor-nw-resize',
  },
];

function WindowResizeHandles() {
  return (
    <>
      {resizeHandles.map(({ direction, className }) => (
        <div
          key={direction}
          aria-hidden="true"
          className={cn('fixed z-[100]', className)}
          onMouseDown={(event) => {
            if (event.button !== 0) return;
            event.preventDefault();
            void appWindow.startResizeDragging(direction);
          }}
        />
      ))}
    </>
  );
}

export default function RendererTitleBar() {
  const clickedHide = () => {
    ipc.sendMessage('window', ['minimize']);
  };

  const clickedResize = () => {
    ipc.sendMessage('window', ['resize']);
  };

  const clickedQuit = () => {
    ipc.sendMessage('window', ['quit']);
  };

  const TitleBarButton = ({
    children,
    className,
    ...props
  }: ComponentProps<'button'>) => {
    return (
      <button
        type="button"
        className={cn(
          'w-8 h-8 bg-transparent border-0 text-white text-base outline-none hover:bg-foreground',
          className,
        )}
        {...props}
      >
        {children}
      </button>
    );
  };

  return (
    <>
      <div
        id="title-bar"
        data-tauri-drag-region
        className="w-full h-[32px] z-50 bg-background flex items-center justify-center px-2 pr-0 absolute top-0 left-0"
      >
        <img
          data-tauri-drag-region
          draggable={false}
          src={icon}
          style={{ width: '20px', height: '20px', marginRight: 8 }}
        />
        <div
          data-tauri-drag-region
          className="text-popover-foreground font-semibold text-sm font-sans select-none"
        >
          Warcraft Recorder
        </div>
        <div id="title-bar-btns" className="ml-auto absolute right-0 top-0">
          <TitleBarButton id="min-btn" onClick={clickedHide}>
            🗕
          </TitleBarButton>
          <TitleBarButton id="max-btn" onClick={clickedResize}>
            🗗
          </TitleBarButton>
          <TitleBarButton
            id="close-btn"
            className="hover:bg-destructive"
            onClick={clickedQuit}
          >
            ✖
          </TitleBarButton>
        </div>
      </div>
      <WindowResizeHandles />
    </>
  );
}
