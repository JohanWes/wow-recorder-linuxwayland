import { useState, useEffect, useRef } from 'react';
import { Download, Loader2, CheckCircle, AlertCircle } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../Dialog/Dialog';
import { Button } from '../Button/Button';
import { getLocalePhrase, Language } from '../../../localisation/translations';
import { Phrase } from '../../../localisation/phrases';

export interface UpdateDialogInfo {
  currentVersion: string;
  latestVersion: string;
  latestReleaseTag: string;
  currentReleaseTag: string;
  releaseUrl: string;
}

interface UpdateProgress {
  stage: string;
  message: string;
}

interface UpdateDialogProps {
  updateInfo: UpdateDialogInfo;
  language: Language;
  onDismiss: (version: string) => void;
  onClose: () => void;
}

interface StageDefinition {
  key: string;
  phraseKey: Phrase;
}

const UPDATE_STAGES: StageDefinition[] = [
  { key: 'downloading', phraseKey: Phrase.UpdateStageDownloading },
  { key: 'verifying', phraseKey: Phrase.UpdateStageVerifying },
  { key: 'installing', phraseKey: Phrase.UpdateStageInstalling },
  { key: 'done', phraseKey: Phrase.UpdateStageRelaunching },
];

const UpdateDialog = ({
  updateInfo,
  language,
  onDismiss,
  onClose,
}: UpdateDialogProps) => {
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const errorFromProgress = useRef(false);

  useEffect(() => {
    const cleanup = window.electron.ipcRenderer.on(
      'updateProgress',
      (evt: unknown) => {
        const p = evt as UpdateProgress;
        setProgress(p);

        if (p.stage === 'error') {
          errorFromProgress.current = true;
          setError(p.message);
          setInstalling(false);
        }
      },
    );
    return cleanup as () => void;
  }, []);

  const handleInstall = async () => {
    setInstalling(true);
    setError(null);
    setProgress(null);
    errorFromProgress.current = false;

    try {
      await window.electron.ipcRenderer.invoke('performUpdate', []);
    } catch (e) {
      if (!errorFromProgress.current) {
        setError(String(e));
        setInstalling(false);
      }
    }
  };

  const handleDismiss = () => {
    onDismiss(updateInfo.latestReleaseTag);
    onClose();
  };

  const isRelaunching =
    progress !== null && progress.stage === 'done' && !error;
  const isActive = installing || isRelaunching;
  const currentStageIdx = progress
    ? UPDATE_STAGES.findIndex((s) => s.key === progress.stage)
    : -1;

  return (
    <Dialog open onOpenChange={(open) => !open && !isActive && onClose()}>
      <DialogContent
        className="sm:max-w-md"
        onPointerDownOutside={(e) => isActive && e.preventDefault()}
        onEscapeKeyDown={(e) => isActive && e.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>
            {isRelaunching
              ? getLocalePhrase(language, Phrase.UpdateInstalledRelaunching)
              : getLocalePhrase(language, Phrase.UpdateAvailableTitle)}
          </DialogTitle>
          <DialogDescription>
            {getLocalePhrase(language, Phrase.UpdateAvailableText)}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-3 py-2">
          <div className="flex items-center justify-between rounded-md border border-white/10 bg-card px-4 py-3">
            <div className="flex flex-col">
              <span className="text-xs text-foreground/60">
                {getLocalePhrase(language, Phrase.Version)}
              </span>
              <span className="font-mono text-sm text-foreground">
                v{updateInfo.currentVersion}
              </span>
            </div>
            <span className="text-foreground/40">→</span>
            <div className="flex flex-col items-end">
              <span className="text-xs text-foreground/60">
                {getLocalePhrase(language, Phrase.Version)}
              </span>
              <span className="font-mono text-sm font-semibold text-green-400">
                v{updateInfo.latestVersion}
              </span>
              {updateInfo.latestReleaseTag && (
                <span className="font-mono text-xs text-foreground/50">
                  {updateInfo.latestReleaseTag}
                </span>
              )}
            </div>
          </div>

          {installing && progress && (
            <div className="flex flex-col gap-2">
              {UPDATE_STAGES.map((stage, idx) => {
                const isComplete = currentStageIdx > idx;
                const isCurrent = currentStageIdx === idx;

                return (
                  <div
                    key={stage.key}
                    className={`flex items-center gap-3 rounded-md px-3 py-2 ${isCurrent ? 'bg-primary/10 border border-primary/30' : ''} ${isComplete ? 'bg-muted/30' : ''}`}
                  >
                    {isComplete ? (
                      <CheckCircle className="h-5 w-5 text-green-400 flex-shrink-0" />
                    ) : isCurrent ? (
                      <Loader2 className="h-5 w-5 text-primary animate-spin flex-shrink-0" />
                    ) : (
                      <div className="h-5 w-5 rounded-full border-2 border-foreground/30 flex-shrink-0" />
                    )}
                    <span
                      className={`text-sm ${
                        isComplete
                          ? 'text-foreground/60'
                          : isCurrent
                            ? 'text-foreground font-medium'
                            : 'text-foreground/40'
                      }`}
                    >
                      {getLocalePhrase(language, stage.phraseKey)}
                    </span>
                  </div>
                );
              })}
              {progress.message && (
                <p className="text-xs text-foreground/50 font-mono truncate">
                  {progress.message}
                </p>
              )}
            </div>
          )}

          {error && (
            <div className="rounded-md bg-destructive/10 border border-destructive/30 px-3 py-2 text-sm text-destructive flex items-start gap-2">
              <AlertCircle className="h-4 w-4 flex-shrink-0 mt-0.5" />
              <span>{error}</span>
            </div>
          )}
        </div>

        <DialogFooter>
          {isRelaunching ? (
            <p className="text-sm text-foreground/60 mr-auto">
              {getLocalePhrase(language, Phrase.UpdateInstalledRelaunching)}
            </p>
          ) : (
            <>
              <Button
                variant="outline"
                onClick={handleDismiss}
                disabled={installing}
              >
                {getLocalePhrase(
                  language,
                  Phrase.UpdateAvailableRemindButtonText,
                )}
              </Button>
              <Button onClick={handleInstall} disabled={installing}>
                {installing ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    {getLocalePhrase(language, Phrase.Preparing)}
                  </>
                ) : (
                  <>
                    <Download className="mr-2 h-4 w-4" />
                    {getLocalePhrase(
                      language,
                      Phrase.UpdateAvailableInstallButtonText,
                    )}
                  </>
                )}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

export default UpdateDialog;
