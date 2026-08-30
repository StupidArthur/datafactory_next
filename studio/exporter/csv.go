package exporter

import (
	"encoding/csv"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"

	"github.com/parquet-go/parquet-go"
)

type CSVOptions struct {
	TitleRows int
	Features  []string
	StartTime *float64
	EndTime   *float64
}

func ParquetToCSV(sourcePath, destinationPath string, options CSVOptions) (resultErr error) {
	source, err := os.Open(sourcePath)
	if err != nil {
		return err
	}
	defer source.Close()
	reader := parquet.NewGenericReader[any](source)
	defer reader.Close()
	destination, err := os.Create(destinationPath)
	if err != nil {
		return err
	}
	defer func() {
		if err := destination.Close(); resultErr == nil {
			resultErr = err
		}
	}()

	columns := append([]string(nil), options.Features...)
	if len(columns) == 0 {
		for _, path := range reader.Schema().Columns() {
			columns = append(columns, path[len(path)-1])
		}
	} else {
		columns = append([]string{"cycle_index", "simulation_time"}, columns...)
	}
	columns = uniqueColumns(columns)
	writer := csv.NewWriter(destination)
	if options.TitleRows > 1 {
		if err := writer.Write([]string{"DataFactory Studio 仿真数据"}); err != nil {
			return err
		}
	}
	if options.TitleRows > 0 {
		if err := writer.Write(columns); err != nil {
			return err
		}
	}

	buffer := make([]any, 1024)
	for {
		count, readErr := reader.Read(buffer)
		for _, item := range buffer[:count] {
			row, ok := item.(map[string]any)
			if !ok {
				return fmt.Errorf("unexpected parquet row type %T", item)
			}
			timeValue, ok := number(row["simulation_time"])
			if !ok {
				return fmt.Errorf("simulation_time is not numeric")
			}
			if options.StartTime != nil && timeValue < *options.StartTime || options.EndTime != nil && timeValue > *options.EndTime {
				continue
			}
			record := make([]string, len(columns))
			for index, column := range columns {
				record[index] = formatValue(row[column])
			}
			if err := writer.Write(record); err != nil {
				return err
			}
		}
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			return readErr
		}
	}
	writer.Flush()
	return writer.Error()
}

func uniqueColumns(columns []string) []string {
	seen := map[string]bool{}
	result := make([]string, 0, len(columns))
	for _, column := range columns {
		if column != "" && !seen[column] {
			seen[column] = true
			result = append(result, column)
		}
	}
	if len(result) == 0 {
		result = []string{"cycle_index", "simulation_time"}
		sort.Strings(result)
	}
	return result
}

func number(value any) (float64, bool) {
	switch value := value.(type) {
	case float64:
		return value, true
	case int64:
		return float64(value), true
	case uint64:
		return float64(value), true
	default:
		return 0, false
	}
}

func formatValue(value any) string {
	switch value := value.(type) {
	case []byte:
		return string(value)
	case float64:
		return strconv.FormatFloat(value, 'g', -1, 64)
	default:
		return fmt.Sprint(value)
	}
}
