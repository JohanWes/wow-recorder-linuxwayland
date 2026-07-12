import { RefreshCw } from 'lucide-react';
import { Language } from 'localisation/translations';
import { getLocalePhrase } from 'localisation/translations';
import { Button } from './components/Button/Button';
import { Tooltip } from './components/Tooltip/Tooltip';
import { Phrase } from 'localisation/phrases';
import { UpdateDialogInfo } from './components/UpdateDialog/UpdateDialog';
import { useState } from 'react';

interface IProps {
  language: Language;
  updateInfo: UpdateDialogInfo | null;
  onCheckForUpdates: () => Promise<void>;
  onUpdateClick: () => void;
}

export default function CheckForUpdatesButton(props: IProps) {
  const { language, updateInfo, onCheckForUpdates, onUpdateClick } = props;
  const [checking, setChecking] = useState(false);

  const handleClick = async () => {
    if (updateInfo) {
      onUpdateClick();
    } else {
      setChecking(true);
      try {
        await onCheckForUpdates();
      } finally {
        setChecking(false);
      }
    }
  };

  const tooltipContent = updateInfo
    ? getLocalePhrase(language, Phrase.UpdateAvailableTooltip)
    : checking
      ? getLocalePhrase(language, Phrase.CheckingForUpdates)
      : getLocalePhrase(language, Phrase.CheckForUpdatesTooltip);

  return (
    <Tooltip content={tooltipContent} side="top">
      <Button
        id="check-updates-button"
        type="button"
        onClick={handleClick}
        variant={updateInfo ? 'destructive' : 'ghost'}
        size="icon"
        disabled={checking}
      >
        <RefreshCw size={20} className={checking ? 'animate-spin' : ''} />
      </Button>
    </Tooltip>
  );
}
