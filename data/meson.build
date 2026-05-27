# Desktop file — rename to match the application_id for Flatpak export.
desktop_conf = configuration_data()
desktop_conf.set('APP_ID', application_id)
desktop_conf.set('ICON', application_id)

desktop_file = configure_file(
  input: 'org.projectbluefin.Finpilot.desktop.in',
  output: application_id + '.desktop',
  configuration: desktop_conf,
)

install_data(
  desktop_file,
  install_dir: datadir / 'applications'
)

if desktop_file_validate.found()
  test(
    'validate-desktop',
    desktop_file_validate,
    args: [ desktop_file ],
  )
endif

# AppStream metainfo — rename to match application_id.
metainfo_conf = configuration_data()
metainfo_conf.set('APP_ID', application_id)

metainfo_file = configure_file(
  input: 'org.projectbluefin.Finpilot.metainfo.xml.in',
  output: application_id + '.metainfo.xml',
  configuration: metainfo_conf,
)

install_data(
  metainfo_file,
  install_dir: datadir / 'metainfo'
)

if appstreamcli.found()
  test(
    'validate-metainfo',
    appstreamcli,
    args: [ 'validate', '--no-net', '--explain', metainfo_file ],
  )
endif

# Icons — install with application_id name so Flatpak export picks them up.
install_data(
  'icons' / 'org.projectbluefin.Finpilot.svg',
  install_dir: iconsdir / 'hicolor' / 'scalable' / 'apps',
  rename: application_id + '.svg',
)

install_data(
  'icons' / 'org.projectbluefin.Finpilot-symbolic.svg',
  install_dir: iconsdir / 'hicolor' / 'symbolic' / 'apps',
  rename: application_id + '-symbolic.svg',
)
