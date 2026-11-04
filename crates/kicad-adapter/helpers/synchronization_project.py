"""Append-only project-class observation for the unchanged legacy board worker."""


def _project_class_main():
    board_path, saved_path = map(Path, sys.argv[1:])
    project_path = board_path.with_suffix('.kicad_pro')
    native_project = saved_path.with_suffix('.kicad_pro')
    output_path = board_path.parent / '.project-netclasses.json'
    require(not output_path.exists() and not output_path.is_symlink(), 'project-class output already exists')
    before_board, before_project = read(board_path), read(project_path)
    serialized = read(native_project)
    require(len(serialized) <= 32 * 1024 * 1024, 'native project output exceeds bound')
    json.loads(serialized)
    board = pcbnew.LoadBoard(str(board_path))
    require(board is not None, 'project-class board load failed')
    settings = board.GetDesignSettings().m_NetSettings
    configured = sorted(board.GetAllNetClasses().items(), key=lambda row: str(row[0]))
    require(0 < len(configured) <= 512, 'configured class inventory exceeds bound')

    def identity(value):
        return None if value is None else dict(name=str(value.GetName()), priority=value.GetPriority())

    def describe(value, effective):
        fields = {}
        for key in ['Clearance', 'TrackWidth', 'ViaDiameter', 'ViaDrill', 'uViaDiameter',
                    'uViaDrill', 'DiffPairWidth', 'DiffPairGap', 'DiffPairViaGap']:
            fields[key] = dict(defined=bool(getattr(value, 'Has' + key)()), value_nm=getattr(value, 'Get' + key)(),
                               parent=identity(getattr(value, 'Get' + key + 'Parent')()))
        result = dict(name=str(value.GetName()), priority=value.GetPriority(), description=str(value.GetDescription()),
                      tuning_profile=dict(defined=bool(value.HasTuningProfile()), value=str(value.GetTuningProfile()),
                                          parent=identity(value.GetTuningProfileParent())), fields=fields)
        if effective:
            result['constituents'] = [identity(value_class) for key, value_class in configured if value.ContainsNetclassWithName(key)]
        return result

    definitions = {str(key): describe(value, False) for key, value in configured}
    names = sorted(str(name) for name in board.GetNetsByName().keys() if str(name))
    require(len(names) <= MAX_RECORDS, 'effective class inventory exceeds bound')
    effective = {name: describe(settings.GetEffectiveNetClass(name), True) for name in names}
    require(read(board_path) == before_board and read(project_path) == before_project, 'project-class inputs changed')
    require(read(native_project) == serialized, 'native serialized project changed')
    result = dict(schema_version=1, native_version=pcbnew.GetBuildVersion(),
                  board_sha256=hashlib.sha256(before_board).hexdigest(), project_sha256=hashlib.sha256(before_project).hexdigest(),
                  native_project_sha256=hashlib.sha256(serialized).hexdigest(), native_project_json=serialized.decode('utf-8'),
                  configured=definitions, effective=effective)
    encoded = json.dumps(result, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode('utf-8')
    require(len(encoded) <= 64 * 1024 * 1024, 'project-class report exceeds bound')
    with output_path.open('xb') as target:
        target.write(encoded)


if __name__ == '__main__':
    _project_class_main()
