/** @jest-environment jsdom */

import '@testing-library/jest-dom';
import { render, screen, fireEvent, act } from '@testing-library/react';
import UpdateDialog, {
  UpdateDialogInfo,
} from '../../renderer/components/UpdateDialog/UpdateDialog';
import { Language } from '../../localisation/phrases';

const mockUpdateInfo: UpdateDialogInfo = {
  currentVersion: '1.0.0',
  latestVersion: '2.0.0',
  latestReleaseTag: 'linux-2.0.0-abc1234',
  currentReleaseTag: 'linux-1.0.0-abc1234',
  releaseUrl: 'https://example.com/release',
};

const createIpcMock = () => {
  const listeners: Record<string, (...args: unknown[]) => void> = {};
  return {
    listeners,
    invoke: jest.fn(),
    on: jest.fn((channel: string, func: (...args: unknown[]) => void) => {
      listeners[channel] = func;
      return jest.fn();
    }),
    sendMessage: jest.fn(),
    removeAllListeners: jest.fn(),
  };
};

describe('UpdateDialog', () => {
  let ipcMock: ReturnType<typeof createIpcMock>;

  beforeEach(() => {
    ipcMock = createIpcMock();
    (window as unknown as Record<string, unknown>).electron = {
      ipcRenderer: ipcMock,
    };
  });

  describe('initial render', () => {
    it('shows version comparison and install button', () => {
      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      expect(screen.getByText('v1.0.0')).toBeInTheDocument();
      expect(screen.getByText('v2.0.0')).toBeInTheDocument();
      expect(
        screen.getByRole('button', { name: /Install Now/i }),
      ).toBeInTheDocument();
    });

    it('shows dismiss button', () => {
      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      expect(
        screen.getByRole('button', { name: /Remind Me Later/i }),
      ).toBeInTheDocument();
    });
  });

  describe('install flow', () => {
    it('shows preparing state on install button after click', async () => {
      ipcMock.invoke.mockResolvedValue(undefined);

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: /Install Now/i }));
      });

      expect(screen.getByText(/Preparing/i)).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Preparing/i })).toBeDisabled();
    });

    it('shows stepper when progress event fires', async () => {
      ipcMock.invoke.mockResolvedValue(undefined);

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: /Install Now/i }));
      });

      await act(async () => {
        ipcMock.listeners['updateProgress']?.({
          stage: 'downloading',
          message: '[install] Downloading WarcraftRecorder.AppImage...',
        });
      });

      expect(screen.getByText(/Downloading update/i)).toBeInTheDocument();
      expect(screen.getByText(/Verifying checksum/i)).toBeInTheDocument();
      expect(screen.getByText(/Installing update/i)).toBeInTheDocument();
      expect(screen.getByText(/Relaunching app/i)).toBeInTheDocument();
    });

    it('shows the raw progress message below the stepper', async () => {
      ipcMock.invoke.mockResolvedValue(undefined);

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: /Install Now/i }));
      });

      await act(async () => {
        ipcMock.listeners['updateProgress']?.({
          stage: 'downloading',
          message: '[install] Downloading WarcraftRecorder.AppImage...',
        });
      });

      expect(
        screen.getByText(/Downloading WarcraftRecorder.AppImage/),
      ).toBeInTheDocument();
    });

    it('shows error state and re-enables buttons', async () => {
      ipcMock.invoke.mockRejectedValue(new Error('install failed'));

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: /Install Now/i }));
      });

      expect(screen.getByText(/install failed/i)).toBeInTheDocument();
      expect(
        screen.getByRole('button', { name: /Install Now/i }),
      ).toBeEnabled();
    });

    it('shows relaunching state when done stage fires', async () => {
      ipcMock.invoke.mockResolvedValue(undefined);

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: /Install Now/i }));
      });

      await act(async () => {
        ipcMock.listeners['updateProgress']?.({
          stage: 'done',
          message: "[install] Done. Run 'warcraftrecorder' to start.",
        });
      });

      expect(
        screen.getAllByText(/Update installed/).length,
      ).toBeGreaterThanOrEqual(1);
      expect(
        screen.queryByRole('button', { name: /Install Now/i }),
      ).not.toBeInTheDocument();
    });
  });

  describe('dismiss', () => {
    it('calls onDismiss and onClose when dismiss button clicked', async () => {
      const onDismiss = jest.fn();
      const onClose = jest.fn();

      render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={onDismiss}
          onClose={onClose}
        />,
      );

      await act(async () => {
        fireEvent.click(
          screen.getByRole('button', { name: /Remind Me Later/i }),
        );
      });

      expect(onDismiss).toHaveBeenCalledWith(mockUpdateInfo.latestReleaseTag);
      expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('registers cleanup for progress listener on unmount', () => {
      const { unmount } = render(
        <UpdateDialog
          updateInfo={mockUpdateInfo}
          language={Language.ENGLISH}
          onDismiss={jest.fn()}
          onClose={jest.fn()}
        />,
      );

      expect(ipcMock.on).toHaveBeenCalledWith(
        'updateProgress',
        expect.any(Function),
      );

      const cleanup = ipcMock.on.mock.results[0].value as jest.Mock;
      unmount();

      expect(cleanup).toHaveBeenCalled();
    });
  });
});
