using System;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;

namespace Cortex.ComputerUse.WindowsFixture
{
    internal sealed class FixtureOptions
    {
        private const int DefaultMaxTimeoutMilliseconds = 86400000;
        private const int MaxStatusLength = 4096;

        public string InitialStatus { get; private set; }
        public string InitialMessage { get; private set; }
        public int? ExitTimeoutMilliseconds { get; private set; }
        public bool NoSecureField { get; private set; }

        private FixtureOptions()
        {
            InitialStatus = "Ready";
            InitialMessage = "";
        }

        public static bool TryParse(string[] args, out FixtureOptions options)
        {
            options = new FixtureOptions();
            for (var index = 0; index < args.Length; index++)
            {
                var argument = args[index];
                if (argument == "--no-secure-field")
                {
                    options.NoSecureField = true;
                    continue;
                }

                if (argument == "--status")
                {
                    if (++index >= args.Length)
                    {
                        return false;
                    }
                    if (args[index].Length > MaxStatusLength)
                    {
                        return false;
                    }
                    options.InitialStatus = args[index];
                    continue;
                }

                if (argument == "--message")
                {
                    if (++index >= args.Length || args[index].Length > MaxStatusLength)
                    {
                        return false;
                    }
                    options.InitialMessage = args[index];
                    continue;
                }

                if (argument == "--exit-timeout-ms")
                {
                    if (++index >= args.Length)
                    {
                        return false;
                    }

                    int timeout;
                    if (!int.TryParse(
                            args[index],
                            System.Globalization.NumberStyles.None,
                            System.Globalization.CultureInfo.InvariantCulture,
                            out timeout) ||
                        timeout < 1 ||
                        timeout > DefaultMaxTimeoutMilliseconds)
                    {
                        return false;
                    }
                    options.ExitTimeoutMilliseconds = timeout;
                    continue;
                }

                return false;
            }

            return true;
        }
    }

    internal sealed class FixtureWindow : Window
    {
        private const string WindowTitle = "Cortex Computer Use Fixture";
        private readonly TextBox messageInput;
        private readonly TextBox verificationStatus;

        public FixtureWindow(FixtureOptions options)
        {
            Title = WindowTitle;
            Width = 640;
            Height = 430;
            ResizeMode = ResizeMode.NoResize;
            WindowStartupLocation = WindowStartupLocation.CenterScreen;
            Background = Brushes.White;

            AutomationProperties.SetAutomationId(this, "computer-use-fixture-window");
            AutomationProperties.SetName(this, WindowTitle);

            var root = new Grid
            {
                Margin = new Thickness(24),
            };
            for (var row = 0; row < 8; row++)
            {
                root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            }
            Content = root;

            var heading = new TextBlock
            {
                Text = WindowTitle,
                FontSize = 20,
                FontWeight = FontWeights.SemiBold,
                Margin = new Thickness(0, 0, 0, 18),
            };
            AutomationProperties.SetAutomationId(heading, "1000");
            AutomationProperties.SetName(heading, WindowTitle);
            Grid.SetRow(heading, 0);
            root.Children.Add(heading);

            var messageLabel = CreateLabel("1005", "Message input");
            Grid.SetRow(messageLabel, 1);
            root.Children.Add(messageLabel);

            messageInput = new TextBox
            {
                Text = options.InitialMessage,
                Height = 30,
                MaxLength = 4096,
                Margin = new Thickness(0, 4, 0, 12),
                VerticalContentAlignment = VerticalAlignment.Center,
            };
            AutomationProperties.SetAutomationId(messageInput, "1001");
            AutomationProperties.SetName(messageInput, "Message input");
            AutomationProperties.SetHelpText(messageInput, "Editable message text");
            AutomationProperties.SetLabeledBy(messageInput, messageLabel);
            Grid.SetRow(messageInput, 2);
            root.Children.Add(messageInput);

            var copyButton = new Button
            {
                Content = "Copy message",
                Width = 126,
                Height = 32,
                HorizontalAlignment = HorizontalAlignment.Right,
                Margin = new Thickness(0, 0, 0, 14),
            };
            AutomationProperties.SetAutomationId(copyButton, "1002");
            AutomationProperties.SetName(copyButton, "Copy message");
            AutomationProperties.SetHelpText(
                copyButton,
                "Copies the message value into the verification status"
            );
            copyButton.Click += ReflectMessageToStatus;
            Grid.SetRow(copyButton, 3);
            root.Children.Add(copyButton);

            // 默认保留安全输入场景；普通交互长跑才显式移除整个密码字段。
            if (!options.NoSecureField)
            {
                var passwordLabel = CreateLabel("1006", "Password input");
                Grid.SetRow(passwordLabel, 4);
                root.Children.Add(passwordLabel);

                var passwordInput = new PasswordBox
                {
                    Height = 30,
                    MaxLength = 1024,
                    Margin = new Thickness(0, 4, 0, 12),
                    VerticalContentAlignment = VerticalAlignment.Center,
                };
                AutomationProperties.SetAutomationId(passwordInput, "1003");
                AutomationProperties.SetName(passwordInput, "Password input");
                AutomationProperties.SetHelpText(passwordInput, "Secure password input");
                AutomationProperties.SetItemStatus(passwordInput, "Password (secure input)");
                AutomationProperties.SetLabeledBy(passwordInput, passwordLabel);
                Grid.SetRow(passwordInput, 5);
                root.Children.Add(passwordInput);
            }

            var verificationLabel = CreateLabel("1007", "Verification status");
            Grid.SetRow(verificationLabel, 6);
            root.Children.Add(verificationLabel);

            verificationStatus = new TextBox
            {
                IsReadOnly = true,
                IsReadOnlyCaretVisible = false,
                AcceptsReturn = false,
                TextWrapping = TextWrapping.Wrap,
                MaxLength = 4096,
                Background = Brushes.WhiteSmoke,
                MinHeight = 30,
                Margin = new Thickness(0, 4, 0, 0),
            };
            AutomationProperties.SetAutomationId(verificationStatus, "1004");
            AutomationProperties.SetName(verificationStatus, "Verification status");
            AutomationProperties.SetHelpText(
                verificationStatus,
                "Read-only status copied from the message input"
            );
            SetVerificationState(options.InitialStatus);
            Grid.SetRow(verificationStatus, 7);
            root.Children.Add(verificationStatus);

            ContentRendered += delegate
            {
                messageInput.Focus();
            };
        }

        public void ScheduleExit(int timeoutMilliseconds)
        {
            var timer = new DispatcherTimer(
                TimeSpan.FromMilliseconds(timeoutMilliseconds),
                DispatcherPriority.Normal,
                delegate
                {
                    Close();
                },
                Dispatcher
            );
            timer.Start();
        }

        private static TextBlock CreateLabel(string automationId, string text)
        {
            var label = new TextBlock
            {
                Text = text,
                FontWeight = FontWeights.SemiBold,
            };
            AutomationProperties.SetAutomationId(label, automationId);
            AutomationProperties.SetName(label, text);
            return label;
        }

        private void ReflectMessageToStatus(object sender, RoutedEventArgs args)
        {
            SetVerificationState(messageInput.Text);
        }

        private void SetVerificationState(string status)
        {
            // 可见文本与 UIA Name 必须逐字相同，验证才能断言精确状态而不是前缀。
            verificationStatus.Text = status;
            AutomationProperties.SetName(verificationStatus, status);
            AutomationProperties.SetItemStatus(verificationStatus, status);
        }
    }

    internal static class Program
    {
        [STAThread]
        private static int Main(string[] args)
        {
            FixtureOptions options;
            if (!FixtureOptions.TryParse(args, out options))
            {
                return 2;
            }

            var application = new Application
            {
                ShutdownMode = ShutdownMode.OnMainWindowClose,
            };
            var window = new FixtureWindow(options);
            if (options.ExitTimeoutMilliseconds.HasValue)
            {
                window.ScheduleExit(options.ExitTimeoutMilliseconds.Value);
            }
            application.Run(window);
            return 0;
        }
    }
}
