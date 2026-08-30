$ErrorActionPreference = 'Stop'
python -m coverage erase
python -W error::ResourceWarning -m coverage run --source=python\dfengine -m unittest discover -s python\tests -v
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
python -m coverage report --fail-under=100 -m
exit $LASTEXITCODE

