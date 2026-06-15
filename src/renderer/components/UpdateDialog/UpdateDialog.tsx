import { useState } from 'react';
import { Download, Loader2 } from 'lucide-react';
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
  releaseUrl: string;
}

interface UpdateDialogProps {
  updateInfo: UpdateDialogInfo;
  language: Language;
  onDismiss: (version: string) => void;
  onClose: () => void;
}

const UpdateDialog = ({
  updateInfo,
  language,
  onDismiss,
  onClose,
}: UpdateDialogProps) => {
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleInstall = async () => {
    setInstalling(true);
    setError(null);

    try {
      await window.electron.ipcRenderer.invoke('performUpdate', []);
    } catch (e) {
      setError(String(e));
      setInstalling(false);
    }
  };

  const handleDismiss = () => {
    onDismiss(updateInfo.latestVersion);
    onClose();
  };

  return (
    <Dialog open onOpenChange={(open) => !open && !installing && onClose()}>
      <DialogContent
        className="sm:max-w-md"
        onPointerDownOutside={(e) => installing && e.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>
            {getLocalePhrase(language, Phrase.UpdateAvailableTitle)}
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
            </div>
          </div>

          {error && (
            <div className="rounded-md bg-destructive/10 border border-destructive/30 px-3 py-2 text-sm text-destructive">
              {error}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={handleDismiss}
            disabled={installing}
          >
            {getLocalePhrase(language, Phrase.UpdateAvailableRemindButtonText)}
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
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

export default UpdateDialog;
