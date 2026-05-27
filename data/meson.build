# Desktop file
desktop_file = files('org.projectbluefin.Finpilot.desktop')
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

# AppStream metainfo
metainfo_file = files('org.projectbluefin.Finpilot.metainfo.xml')
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

# Icons
install_data(
  'icons' / 'org.projectbluefin.Finpilot.svg',
  install_dir: iconsdir / 'hicolor' / 'scalable' / 'apps'
)

install_data(
  'icons' / 'org.projectbluefin.Finpilot-symbolic.svg',
  install_dir: iconsdir / 'hicolor' / 'symbolic' / 'apps'
)
