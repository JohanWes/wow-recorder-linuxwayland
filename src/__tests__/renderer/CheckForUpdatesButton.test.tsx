/** @jest-environment jsdom */

import '@testing-library/jest-dom';
import { render, screen, fireEvent, act } from '@testing-library/react';
import CheckForUpdatesButton from '../../renderer/CheckForUpdatesButton';
import { TooltipProvider } from '../../renderer/components/Tooltip/Tooltip';
import { Language } from '../../localisation/phrases';
import { UpdateDialogInfo } from '../../renderer/components/UpdateDialog/UpdateDialog';

const mockUpdateInfo: UpdateDialogInfo = {
  currentVersion: '1.0.0',
  latestVersion: '2.0.0',
  latestReleaseTag: 'linux-2.0.0-abc1234',
  currentReleaseTag: 'linux-1.0.0-abc1234',
  releaseUrl: 'https://example.com/release',
};

const wrapper = ({ children }: { children: React.ReactNode }) => (
  <TooltipProvider>{children}</TooltipProvider>
);

describe('CheckForUpdatesButton', () => {
  beforeEach(() => {
    (window as unknown as Record<string, unknown>).electron = {
      ipcRenderer: {
        sendMessage: jest.fn(),
        invoke: jest.fn(),
        on: jest.fn(),
        removeAllListeners: jest.fn(),
      },
    };
  });

  it('renders ghost variant when no update info', () => {
    render(
      <CheckForUpdatesButton
        language={Language.ENGLISH}
        updateInfo={null}
        onCheckForUpdates={jest.fn()}
        onUpdateClick={jest.fn()}
      />,
      { wrapper },
    );

    const button = screen.getByRole('button');
    expect(button).not.toHaveClass('bg-destructive');
  });

  it('renders destructive variant when update info is present', () => {
    render(
      <CheckForUpdatesButton
        language={Language.ENGLISH}
        updateInfo={mockUpdateInfo}
        onCheckForUpdates={jest.fn()}
        onUpdateClick={jest.fn()}
      />,
      { wrapper },
    );

    const button = screen.getByRole('button');
    expect(button.className).toContain('bg-destructive');
  });

  it('calls onCheckForUpdates when clicked with no update info', async () => {
    const onCheckForUpdates = jest.fn().mockResolvedValue(undefined);
    const onUpdateClick = jest.fn();

    render(
      <CheckForUpdatesButton
        language={Language.ENGLISH}
        updateInfo={null}
        onCheckForUpdates={onCheckForUpdates}
        onUpdateClick={onUpdateClick}
      />,
      { wrapper },
    );

    await act(async () => {
      fireEvent.click(screen.getByRole('button'));
    });

    expect(onCheckForUpdates).toHaveBeenCalledTimes(1);
    expect(onUpdateClick).not.toHaveBeenCalled();
  });

  it('calls onUpdateClick when clicked with update info', () => {
    const onCheckForUpdates = jest.fn();
    const onUpdateClick = jest.fn();

    render(
      <CheckForUpdatesButton
        language={Language.ENGLISH}
        updateInfo={mockUpdateInfo}
        onCheckForUpdates={onCheckForUpdates}
        onUpdateClick={onUpdateClick}
      />,
      { wrapper },
    );

    fireEvent.click(screen.getByRole('button'));

    expect(onUpdateClick).toHaveBeenCalledTimes(1);
    expect(onCheckForUpdates).not.toHaveBeenCalled();
  });

  it('disables button while checking', async () => {
    let resolveCheck: () => void;
    const onCheckForUpdates = jest.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveCheck = resolve;
        }),
    );

    render(
      <CheckForUpdatesButton
        language={Language.ENGLISH}
        updateInfo={null}
        onCheckForUpdates={onCheckForUpdates}
        onUpdateClick={jest.fn()}
      />,
      { wrapper },
    );

    const button = screen.getByRole('button');

    await act(async () => {
      fireEvent.click(button);
    });

    expect(button).toBeDisabled();

    await act(async () => {
      resolveCheck!();
    });
  });
});
