import React, { Dispatch, SetStateAction } from 'react';
import { AdvancedLoggingStatus, AppState, RecStatus } from 'main/types';
import { ConfigurationSchema } from 'config/configSchema';
import { getLocalePhrase } from 'localisation/translations';
import GeneralSettings from './GeneralSettings';
import FlavourSettings from './FlavourSettings';
import PVESettings from './PVESettings';
import PVPSettings from './PVPSettings';
import {
  Tabs,
  TabsList,
  TabsContent,
  TabsTrigger,
} from './components/Tabs/Tabs';
import Separator from './components/Separator/Separator';
import { ScrollArea } from './components/ScrollArea/ScrollArea';
import LocaleSettings from './LocaleSettings';
import { Phrase } from 'localisation/phrases';
import LinuxCaptureSettings from './LinuxCaptureSettings';

interface IProps {
  recorderStatus: RecStatus;
  config: ConfigurationSchema;
  setConfig: Dispatch<SetStateAction<ConfigurationSchema>>;
  appState: AppState;
  setAppState: React.Dispatch<React.SetStateAction<AppState>>;
  advancedLoggingStatus: AdvancedLoggingStatus;
}

const CategoryHeading = ({ children }: { children: React.ReactNode }) => (
  <h2 className="text-foreground-lighter font-bold">{children}</h2>
);

const SettingsPage: React.FC<IProps> = (props: IProps) => {
  const {
    recorderStatus,
    config,
    setConfig,
    appState,
    setAppState,
    advancedLoggingStatus,
  } = props;

  return (
    <div className="w-full h-full bg-background-higher pt-[32px] px-4">
      <Tabs defaultValue="application" className="w-full">
        <TabsList>
          <TabsTrigger value="application">
            {getLocalePhrase(
              appState.language,
              Phrase.SettingsPageApplicationHeader,
            )}
          </TabsTrigger>
          <TabsTrigger value="game">
            {getLocalePhrase(appState.language, Phrase.SettingsPageGameHeader)}
          </TabsTrigger>
        </TabsList>
        <ScrollArea
          withScrollIndicators={false}
          className="h-[calc(100svh-48px)] pb-8"
        >
          <TabsContent value="application">
            <div className="p-4 flex flex-col gap-y-8">
              <div>
                <CategoryHeading>
                  {getLocalePhrase(
                    appState.language,
                    Phrase.GeneralSettingsLabel,
                  )}
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <GeneralSettings
                  recorderStatus={recorderStatus}
                  appState={appState}
                />
              </div>
              <div>
                <CategoryHeading>
                  Linux Capture
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <LinuxCaptureSettings
                  recorderStatus={recorderStatus}
                  config={config}
                  setConfig={setConfig}
                />
              </div>
              <div>
                <CategoryHeading>
                  {getLocalePhrase(
                    appState.language,
                    Phrase.LocaleSettingsLabel,
                  )}
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <LocaleSettings
                  config={config}
                  setConfig={setConfig}
                  appState={appState}
                  setAppState={setAppState}
                />
              </div>
            </div>
          </TabsContent>

          <TabsContent value="game">
            <div className="p-4 flex flex-col gap-y-8">
              <div>
                <CategoryHeading>
                  {getLocalePhrase(appState.language, Phrase.GameSettingsLabel)}
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <FlavourSettings
                  recorderStatus={recorderStatus}
                  config={config}
                  setConfig={setConfig}
                  appState={appState}
                  advancedLoggingStatus={advancedLoggingStatus}
                />
              </div>
              <div>
                <CategoryHeading>
                  {getLocalePhrase(appState.language, Phrase.PVESettingsLabel)}
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <PVESettings appState={appState} />
              </div>
              <div>
                <CategoryHeading>
                  {getLocalePhrase(appState.language, Phrase.PVPSettingsLabel)}
                </CategoryHeading>
                <Separator className="mt-2 mb-4" />
                <PVPSettings appState={appState} />
              </div>
            </div>
          </TabsContent>
        </ScrollArea>
      </Tabs>
    </div>
  );
};

export default SettingsPage;
